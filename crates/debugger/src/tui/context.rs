//! Debugger context and event handler implementation.

use crate::{DebugNode, ExitReason, debugger::DebuggerContext};
use alloy_primitives::{Address, hex};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use foundry_evm_core::buffer::BufferKind;
use revm::bytecode::opcode::OpCode;
use revm_inspectors::tracing::types::{CallKind, CallTraceStep};
use std::ops::ControlFlow;

/// This is currently used to remember last scroll position so screen doesn't wiggle as much.
#[derive(Default)]
pub(crate) struct DrawMemory {
    pub(crate) inner_call_index: usize,
    pub(crate) current_buf_startline: usize,
    pub(crate) current_stack_startline: usize,
}

pub(crate) struct TUIContext<'a> {
    pub(crate) debugger_context: &'a mut DebuggerContext,

    /// Buffer for keys prior to execution, i.e. '10' + 'k' => move up 10 operations.
    pub(crate) key_buffer: String,
    /// Current step in the debug steps.
    pub(crate) current_step: usize,
    pub(crate) draw_memory: DrawMemory,
    pub(crate) opcode_list: Vec<String>,
    pub(crate) last_index: usize,

    pub(crate) stack_labels: bool,
    /// Whether to decode active buffer as utf8 or not.
    pub(crate) buf_utf: bool,
    pub(crate) show_shortcuts: bool,
    /// The currently active buffer (memory, calldata, returndata) to be drawn.
    pub(crate) active_buffer: BufferKind,
}

impl<'a> TUIContext<'a> {
    pub(crate) fn new(debugger_context: &'a mut DebuggerContext) -> Self {
        TUIContext {
            debugger_context,

            key_buffer: String::with_capacity(64),
            current_step: 0,
            draw_memory: DrawMemory::default(),
            opcode_list: Vec::new(),
            last_index: 0,

            stack_labels: false,
            buf_utf: false,
            show_shortcuts: true,
            active_buffer: BufferKind::Memory,
        }
    }

    pub(crate) fn init(&mut self) {
        self.gen_opcode_list();
    }

    pub(crate) fn debug_arena(&self) -> &[DebugNode] {
        &self.debugger_context.debug_arena
    }

    pub(crate) fn debug_call(&self) -> &DebugNode {
        &self.debug_arena()[self.draw_memory.inner_call_index]
    }

    /// Returns the current call address.
    pub(crate) fn address(&self) -> &Address {
        &self.debug_call().address
    }

    /// Returns the current call kind.
    pub(crate) fn call_kind(&self) -> CallKind {
        self.debug_call().kind
    }

    /// Returns the current debug steps.
    pub(crate) fn debug_steps(&self) -> &[CallTraceStep] {
        &self.debug_call().steps
    }

    /// Returns the current debug step.
    pub(crate) fn current_step(&self) -> &CallTraceStep {
        &self.debug_steps()[self.current_step]
    }

    fn gen_opcode_list(&mut self) {
        self.opcode_list.clear();
        let debug_steps =
            &self.debugger_context.debug_arena[self.draw_memory.inner_call_index].steps;
        for step in debug_steps {
            self.opcode_list.push(pretty_opcode(step));
        }
    }

    fn gen_opcode_list_if_necessary(&mut self) {
        if self.last_index != self.draw_memory.inner_call_index {
            self.gen_opcode_list();
            self.last_index = self.draw_memory.inner_call_index;
        }
    }

    fn active_buffer(&self) -> &[u8] {
        match self.active_buffer {
            BufferKind::Memory => self.current_step().memory.as_ref().unwrap().as_bytes(),
            BufferKind::Calldata => &self.debug_call().calldata,
            BufferKind::Returndata => &self.current_step().returndata,
        }
    }
}

impl TUIContext<'_> {
    pub(crate) fn handle_event(&mut self, event: Event) -> ControlFlow<ExitReason> {
        let ret = match event {
            Event::Key(event) => self.handle_key_event(event),
            Event::Mouse(event) => self.handle_mouse_event(event),
            _ => ControlFlow::Continue(()),
        };
        // Generate the list after the event has been handled.
        self.gen_opcode_list_if_necessary();
        ret
    }

    fn handle_key_event(&mut self, event: KeyEvent) -> ControlFlow<ExitReason> {
        // Breakpoints
        if let KeyCode::Char(c) = event.code
            && c.is_alphabetic()
            && self.key_buffer.starts_with('\'')
        {
            self.handle_breakpoint(c);
            return ControlFlow::Continue(());
        }

        let control = event.modifiers.contains(KeyModifiers::CONTROL);

        match event.code {
            // Exit
            KeyCode::Char('q') => return ControlFlow::Break(ExitReason::CharExit),

            // Scroll up the memory buffer
            KeyCode::Char('k') | KeyCode::Up if control => self.repeat(|this| {
                this.draw_memory.current_buf_startline =
                    this.draw_memory.current_buf_startline.saturating_sub(1);
            }),
            // Scroll down the memory buffer
            KeyCode::Char('j') | KeyCode::Down if control => self.repeat(|this| {
                let max_buf = (this.active_buffer().len() / 32).saturating_sub(1);
                if this.draw_memory.current_buf_startline < max_buf {
                    this.draw_memory.current_buf_startline += 1;
                }
            }),

            // Move up
            KeyCode::Char('k') | KeyCode::Up => self.repeat(Self::step_back),
            // Move down
            KeyCode::Char('j') | KeyCode::Down => self.repeat(Self::step),

            // Scroll up the stack
            KeyCode::Char('K') => self.repeat(|this| {
                this.draw_memory.current_stack_startline =
                    this.draw_memory.current_stack_startline.saturating_sub(1);
            }),
            // Scroll down the stack
            KeyCode::Char('J') => self.repeat(|this| {
                let max_stack =
                    this.current_step().stack.as_ref().map_or(0, |s| s.len()).saturating_sub(1);
                if this.draw_memory.current_stack_startline < max_stack {
                    this.draw_memory.current_stack_startline += 1;
                }
            }),

            // Cycle buffers
            KeyCode::Char('b') => {
                self.active_buffer = self.active_buffer.next();
                self.draw_memory.current_buf_startline = 0;
            }

            // Go to top of file
            KeyCode::Char('g') => {
                self.draw_memory.inner_call_index = 0;
                self.current_step = 0;
            }

            // Go to bottom of file
            KeyCode::Char('G') => {
                self.draw_memory.inner_call_index = self.debug_arena().len() - 1;
                self.current_step = self.n_steps() - 1;
            }

            // Go to previous call
            KeyCode::Char('c') => {
                self.draw_memory.inner_call_index =
                    self.draw_memory.inner_call_index.saturating_sub(1);
                self.current_step = self.n_steps() - 1;
            }

            // Go to next call
            KeyCode::Char('C') => {
                if self.debug_arena().len() > self.draw_memory.inner_call_index + 1 {
                    self.draw_memory.inner_call_index += 1;
                    self.current_step = 0;
                }
            }

            // Step forward
            KeyCode::Char('s') => self.repeat(|this| {
                let remaining_steps = &this.debug_steps()[this.current_step..];
                if let Some((i, _)) =
                    remaining_steps.iter().enumerate().skip(1).find(|(i, step)| {
                        let prev = &remaining_steps[*i - 1];
                        is_jump(step, prev)
                    })
                {
                    this.current_step += i
                }
            }),

            // Step backwards
            KeyCode::Char('a') => self.repeat(|this| {
                let ops = &this.debug_steps()[..this.current_step];
                this.current_step = ops
                    .iter()
                    .enumerate()
                    .skip(1)
                    .rev()
                    .find(|&(i, op)| {
                        let prev = &ops[i - 1];
                        is_jump(op, prev)
                    })
                    .map(|(i, _)| i)
                    .unwrap_or_default();
            }),

            // Toggle stack labels
            KeyCode::Char('t') => self.stack_labels = !self.stack_labels,

            // Toggle memory UTF-8 decoding
            KeyCode::Char('m') => self.buf_utf = !self.buf_utf,

            // Toggle help notice
            KeyCode::Char('h') => self.show_shortcuts = !self.show_shortcuts,

            // Numbers for repeating commands or breakpoints
            KeyCode::Char(
                other @ ('0' | '1' | '2' | '3' | '4' | '5' | '6' | '7' | '8' | '9' | '\''),
            ) => {
                // Early return to not clear the buffer.
                self.key_buffer.push(other);
                return ControlFlow::Continue(());
            }

            // Unknown/unhandled key code
            _ => {}
        };

        self.key_buffer.clear();
        ControlFlow::Continue(())
    }

    fn handle_breakpoint(&mut self, c: char) {
        // Find the location of the called breakpoint in the whole debug arena (at this address with
        // this pc)
        if let Some((caller, pc)) = self.debugger_context.breakpoints.get(&c) {
            for (i, node) in self.debug_arena().iter().enumerate() {
                if node.address == *caller
                    && let Some(step) = node.steps.iter().position(|step| step.pc == *pc)
                {
                    self.draw_memory.inner_call_index = i;
                    self.current_step = step;
                    break;
                }
            }
        }
        self.key_buffer.clear();
    }

    fn handle_mouse_event(&mut self, event: MouseEvent) -> ControlFlow<ExitReason> {
        match event.kind {
            MouseEventKind::ScrollUp => self.step_back(),
            MouseEventKind::ScrollDown => self.step(),
            _ => {}
        }

        ControlFlow::Continue(())
    }

    fn step_back(&mut self) {
        if self.current_step > 0 {
            self.current_step -= 1;
        } else if self.draw_memory.inner_call_index > 0 {
            self.draw_memory.inner_call_index -= 1;
            self.current_step = self.n_steps() - 1;
        }
    }

    fn step(&mut self) {
        if self.current_step < self.n_steps() - 1 {
            self.current_step += 1;
        } else if self.draw_memory.inner_call_index < self.debug_arena().len() - 1 {
            self.draw_memory.inner_call_index += 1;
            self.current_step = 0;
        }
    }

    /// Calls a closure `f` the number of times specified in the key buffer, and at least once.
    fn repeat(&mut self, mut f: impl FnMut(&mut Self)) {
        for _ in 0..buffer_as_number(&self.key_buffer) {
            f(self);
        }
    }

    fn n_steps(&self) -> usize {
        self.debug_steps().len()
    }
}

/// Grab number from buffer. Used for something like '10k' to move up 10 operations
fn buffer_as_number(s: &str) -> usize {
    const MIN: usize = 1;
    const MAX: usize = 100_000;
    s.parse().unwrap_or(MIN).clamp(MIN, MAX)
}

fn pretty_opcode(step: &CallTraceStep) -> String {
    if let Some(immediate) = step.immediate_bytes.as_ref().filter(|b| !b.is_empty()) {
        format!("{}(0x{})", step.op, hex::encode(immediate))
    } else {
        step.op.to_string()
    }
}

fn is_jump(step: &CallTraceStep, prev: &CallTraceStep) -> bool {
    if !matches!(prev.op, OpCode::JUMP | OpCode::JUMPI) {
        return false;
    }

    let immediate_len = prev.immediate_bytes.as_ref().map_or(0, |b| b.len());

    step.pc != prev.pc + 1 + immediate_len
}
