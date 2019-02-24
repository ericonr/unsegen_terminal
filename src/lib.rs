extern crate libc;
extern crate nix;
extern crate unsegen;
extern crate vte;
#[allow(dead_code)]
mod ansi;
#[allow(dead_code)]
mod index;
mod pty;
mod terminalwindow;

use ansi::Processor;
use pty::{PTYInput, PTYOutput, PTY};
use std::ffi::{OsStr, OsString};
use unsegen::base::basic_types::*;
use unsegen::base::Window;
use unsegen::container::Container;
use unsegen::input::{Behavior, Input, Key, OperationResult, ScrollBehavior, Scrollable, Writable};
use unsegen::widget::{Demand2D, RenderingHints, Widget};

use terminalwindow::DualWindow;

use std::cell::RefCell;
use std::fs::File;
use std::thread;

fn read_slave_input_loop<S: SlaveInputSink>(sink: S, mut reader: PTYOutput) {
    use std::io::Read;

    let mut buffer = [0; 1024];
    while let Ok(n) = reader.read(&mut buffer) {
        let mut bytes = vec![0; n];
        bytes.copy_from_slice(&mut buffer[..n]);
        sink.send(bytes.into_boxed_slice());
    }
}
// Sink that receives all (byte) input that is send from a slave terminal
pub trait SlaveInputSink: std::marker::Send {
    fn send(&self, data: Box<[u8]>);
}

// Passes all inputs through to the modelled terminal
pub struct PassthroughBehavior<'a> {
    term: &'a mut Terminal,
}

impl<'a> PassthroughBehavior<'a> {
    pub fn new(term: &'a mut Terminal) -> Self {
        PassthroughBehavior { term: term }
    }
}

impl<'a> Behavior for PassthroughBehavior<'a> {
    fn input(self, i: Input) -> Option<Input> {
        self.term.process_input(i);
        None
    }
}

pub struct Terminal {
    terminal_window: RefCell<DualWindow>,
    //slave_input_thread: thread::Thread,
    master_input_sink: RefCell<PTYInput>,

    // Hack used to keep the slave device open as long as the master exists.
    // This may not be a good idea, we will see...
    _slave_handle: File,
    slave_name: OsString,

    ansi_processor: Processor,
}

impl Terminal {
    pub fn new<S: SlaveInputSink + 'static>(input_sink: S) -> Self {
        let process_pty = PTY::open().expect("Could not create pty.");

        let ptsname = process_pty.name().to_owned();

        let (pty_input, pty_output) = process_pty.split_io();

        /*let slave_input_thread =*/
        thread::Builder::new()
            .name("slave input thread".to_owned())
            .spawn(move || {
                read_slave_input_loop(input_sink, pty_output);
            })
            .expect("Spawn slave input thread");

        // Hack:
        // Open slave terminal, so that it does not get destroyed when a gdb process opens it and
        // closes it afterwards.
        let mut pts = std::fs::OpenOptions::new()
            .write(true)
            .read(true)
            .open(&ptsname)
            .expect("pts file");
        use std::io::Write;
        write!(pts, "").expect("initial write to pts");

        Terminal {
            terminal_window: RefCell::new(DualWindow::new()),
            master_input_sink: RefCell::new(pty_input),
            //slave_input_thread: slave_input_thread,
            _slave_handle: pts,
            slave_name: ptsname,
            ansi_processor: Processor::new(),
        }
    }

    //TODO: do we need to distinguish between input from user and from slave?
    pub fn add_byte_input(&mut self, bytes: Box<[u8]>) {
        use std::ops::DerefMut;
        let mut window_ref = self.terminal_window.borrow_mut();
        let mut sink_ref = self.master_input_sink.borrow_mut();
        for byte in bytes.iter() {
            self.ansi_processor
                .advance(window_ref.deref_mut(), *byte, sink_ref.deref_mut());
        }
    }

    pub fn get_slave_name(&self) -> &OsStr {
        self.slave_name.as_ref()
    }

    pub fn process_input(&mut self, i: Input) {
        use std::io::Write;
        self.master_input_sink
            .borrow_mut()
            .write_all(i.raw.as_slice())
            .expect("Write to terminal");
    }

    fn ensure_size(&self, w: Width, h: Height) {
        let mut window = self.terminal_window.borrow_mut();
        if w != window.get_width() || h != window.get_height() {
            window.set_width(w);
            window.set_height(h);

            let w16 = w.raw_value() as u16;
            let h16 = h.raw_value() as u16;
            self.master_input_sink
                .borrow_mut()
                .resize(w16, h16, w16 /* TODO ??*/, h16 /* TODO ??*/)
                .expect("Resize pty");
        }
    }
}

impl Writable for Terminal {
    fn write(&mut self, c: char) -> OperationResult {
        use std::io::Write;
        write!(self.master_input_sink.borrow_mut(), "{}", c).expect("Write key to terminal");
        Ok(())
    }
}

impl Scrollable for Terminal {
    fn scroll_forwards(&mut self) -> OperationResult {
        self.terminal_window.borrow_mut().scroll_forwards()
    }
    fn scroll_backwards(&mut self) -> OperationResult {
        self.terminal_window.borrow_mut().scroll_backwards()
    }
    fn scroll_to_beginning(&mut self) -> OperationResult {
        self.terminal_window.borrow_mut().scroll_to_beginning()
    }
    fn scroll_to_end(&mut self) -> OperationResult {
        self.terminal_window.borrow_mut().scroll_to_end()
    }
}

impl Widget for Terminal {
    fn space_demand(&self) -> Demand2D {
        self.terminal_window.borrow().space_demand()
    }
    fn draw(&self, window: Window, hints: RenderingHints) {
        self.ensure_size(window.get_width(), window.get_height());
        self.terminal_window.borrow_mut().draw(window, hints);
    }
}

impl<P: ?Sized> Container<P> for Terminal {
    fn input(&mut self, input: Input, _: &mut P) -> Option<Input> {
        input
            .chain(
                ScrollBehavior::new(self)
                    .forwards_on(Key::PageDown)
                    .backwards_on(Key::PageUp)
                    .to_beginning_on(Key::Home)
                    .to_end_on(Key::End),
            )
            .chain(PassthroughBehavior::new(self))
            .finish()
    }
}
