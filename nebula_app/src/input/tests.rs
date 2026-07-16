use std::time::{Duration, Instant};

use winit::event::{
    DeviceId, ElementState, Event as WinitEvent, Modifiers, MouseButton, WindowEvent,
};
#[cfg(target_os = "macos")]
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, ModifiersState};
use winit::window::WindowId;

use nebula_terminal::event::{Event as TerminalEvent, EventListener};
use nebula_terminal::grid::Scroll;
use nebula_terminal::index::{Direction, Point, Side};
use nebula_terminal::term::Term;
use nebula_terminal::term::search::Match;

use crate::clipboard::Clipboard;
use crate::config::{Action, Binding, BindingMode, UiConfig};
use crate::display::window::Window;
use crate::display::{Display, SizeInfo};
use crate::event::{ClickState, InlineSearchState, Mouse, TouchPurpose};
use crate::message_bar::{Message, MessageBuffer};
use crate::scheduler::Scheduler;

use super::chrome::multi_click_time;
use super::{ActionContext as InputActionContext, Processor};

const KEY: Key<&'static str> = Key::Character("0");

struct MockEventProxy;
impl EventListener for MockEventProxy {}

struct ActionContext<'a, T> {
    pub terminal: &'a mut Term<T>,
    pub size_info: &'a SizeInfo,
    pub mouse: &'a mut Mouse,
    pub clipboard: &'a mut Clipboard,
    pub message_buffer: &'a mut MessageBuffer,
    pub modifiers: Modifiers,
    config: &'a UiConfig,
    inline_search_state: &'a mut InlineSearchState,
}

impl<T: EventListener> InputActionContext<T> for ActionContext<'_, T> {
    fn search_next(&mut self, _origin: Point, _direction: Direction, _side: Side) -> Option<Match> {
        None
    }

    fn search_direction(&self) -> Direction {
        Direction::Right
    }

    fn inline_search_state(&mut self) -> &mut InlineSearchState {
        self.inline_search_state
    }

    fn search_active(&self) -> bool {
        false
    }

    fn terminal(&self) -> &Term<T> {
        self.terminal
    }

    fn terminal_mut(&mut self) -> &mut Term<T> {
        self.terminal
    }

    fn size_info(&self) -> SizeInfo {
        *self.size_info
    }

    fn selection_is_empty(&self) -> bool {
        true
    }

    fn scroll(&mut self, scroll: Scroll) {
        self.terminal.scroll_display(scroll);
    }

    fn mouse_mode(&self) -> bool {
        false
    }

    #[inline]
    fn mouse_mut(&mut self) -> &mut Mouse {
        self.mouse
    }

    #[inline]
    fn mouse(&self) -> &Mouse {
        self.mouse
    }

    #[inline]
    fn touch_purpose(&mut self) -> &mut TouchPurpose {
        unimplemented!();
    }

    fn modifiers(&mut self) -> &mut Modifiers {
        &mut self.modifiers
    }

    fn window(&mut self) -> &mut Window {
        unimplemented!();
    }

    fn display(&mut self) -> &mut Display {
        unimplemented!();
    }

    fn nebula_chrome_active(&self) -> bool {
        false
    }

    fn pop_message(&mut self) {
        self.message_buffer.pop();
    }

    fn message(&self) -> Option<&Message> {
        self.message_buffer.message()
    }

    fn config(&self) -> &UiConfig {
        self.config
    }

    fn clipboard_mut(&mut self) -> &mut Clipboard {
        self.clipboard
    }

    #[cfg(target_os = "macos")]
    fn event_loop(&self) -> &ActiveEventLoop {
        unimplemented!();
    }

    fn scheduler_mut(&mut self) -> &mut Scheduler {
        unimplemented!();
    }

    fn semantic_word(&self, _point: Point) -> String {
        unimplemented!();
    }
}

macro_rules! test_clickstate {
        {
            name: $name:ident,
            initial_state: $initial_state:expr,
            initial_button: $initial_button:expr,
            input: $input:expr,
            end_state: $end_state:expr,
            input_delay: $input_delay:expr,
        } => {
            #[test]
            fn $name() {
                let mut clipboard = Clipboard::new_nop();
                let cfg = UiConfig::default();
                let size = SizeInfo::new(
                    21.0,
                    51.0,
                    3.0,
                    3.0,
                    0.,
                    0.,
                    false,
                );

                let mut terminal = Term::new(cfg.term_options(), &size, MockEventProxy);

                let mut mouse = Mouse {
                    click_state: $initial_state,
                    last_click_button: $initial_button,
                    last_click_timestamp: Instant::now() - $input_delay,
                    ..Mouse::default()
                };

                let mut inline_search_state = InlineSearchState::default();
                let mut message_buffer = MessageBuffer::default();

                let context = ActionContext {
                    terminal: &mut terminal,
                    mouse: &mut mouse,
                    size_info: &size,
                    clipboard: &mut clipboard,
                    modifiers: Default::default(),
                    message_buffer: &mut message_buffer,
                    inline_search_state: &mut inline_search_state,
                    config: &cfg,
                };

                let mut processor = Processor::new(context);

                let event: WinitEvent::<TerminalEvent> = $input;
                if let WinitEvent::WindowEvent {
                    event: WindowEvent::MouseInput {
                        state,
                        button,
                        ..
                    },
                    ..
                } = event
                {
                    processor.mouse_input(state, button);
                };

                assert_eq!(processor.ctx.mouse.click_state, $end_state);
            }
        }
    }

macro_rules! test_process_binding {
        {
            name: $name:ident,
            binding: $binding:expr,
            triggers: $triggers:expr,
            mode: $mode:expr,
            mods: $mods:expr,
        } => {
            #[test]
            fn $name() {
                if $triggers {
                    assert!($binding.is_triggered_by($mode, $mods, &KEY));
                } else {
                    assert!(!$binding.is_triggered_by($mode, $mods, &KEY));
                }
            }
        }
    }

test_clickstate! {
    name: single_click,
    initial_state: ClickState::None,
    initial_button: MouseButton::Other(0),
    input: WinitEvent::WindowEvent {
        event: WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            device_id: DeviceId::dummy(),
        },
        window_id: WindowId::dummy(),
    },
    end_state: ClickState::Click,
    input_delay: Duration::ZERO,
}

test_clickstate! {
    name: single_right_click,
    initial_state: ClickState::None,
    initial_button: MouseButton::Other(0),
    input: WinitEvent::WindowEvent {
        event: WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Right,
            device_id: DeviceId::dummy(),
        },
        window_id: WindowId::dummy(),
    },
    end_state: ClickState::Click,
    input_delay: Duration::ZERO,
}

test_clickstate! {
    name: single_middle_click,
    initial_state: ClickState::None,
    initial_button: MouseButton::Other(0),
    input: WinitEvent::WindowEvent {
        event: WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Middle,
            device_id: DeviceId::dummy(),
        },
        window_id: WindowId::dummy(),
    },
    end_state: ClickState::Click,
    input_delay: Duration::ZERO,
}

test_clickstate! {
    name: double_click,
    initial_state: ClickState::Click,
    initial_button: MouseButton::Left,
    input: WinitEvent::WindowEvent {
        event: WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            device_id: DeviceId::dummy(),
        },
        window_id: WindowId::dummy(),
    },
    end_state: ClickState::DoubleClick,
    input_delay: Duration::ZERO,
}

test_clickstate! {
    name: double_click_failed,
    initial_state: ClickState::Click,
    initial_button: MouseButton::Left,
    input: WinitEvent::WindowEvent {
        event: WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            device_id: DeviceId::dummy(),
        },
        window_id: WindowId::dummy(),
    },
    end_state: ClickState::Click,
    input_delay: multi_click_time(),
}

test_clickstate! {
    name: triple_click,
    initial_state: ClickState::DoubleClick,
    initial_button: MouseButton::Left,
    input: WinitEvent::WindowEvent {
        event: WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            device_id:  DeviceId::dummy(),
        },
        window_id:  WindowId::dummy(),
    },
    end_state: ClickState::TripleClick,
    input_delay: Duration::ZERO,
}

test_clickstate! {
    name: triple_click_failed,
    initial_state: ClickState::DoubleClick,
    initial_button: MouseButton::Left,
    input: WinitEvent::WindowEvent {
        event: WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            device_id: DeviceId::dummy(),
        },
        window_id: WindowId::dummy(),
    },
    end_state: ClickState::Click,
    input_delay: multi_click_time(),
}

test_clickstate! {
    name: multi_click_separate_buttons,
    initial_state: ClickState::DoubleClick,
    initial_button: MouseButton::Left,
    input: WinitEvent::WindowEvent {
        event: WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Right,
            device_id: DeviceId::dummy(),
        },
        window_id: WindowId::dummy(),
    },
    end_state: ClickState::Click,
    input_delay: Duration::ZERO,
}

test_process_binding! {
    name: process_binding_nomode_shiftmod_require_shift,
    binding: Binding { trigger: KEY, mods: ModifiersState::SHIFT, action: Action::from("\x1b[1;2D"), mode: BindingMode::empty(), notmode: BindingMode::empty() },
    triggers: true,
    mode: BindingMode::empty(),
    mods: ModifiersState::SHIFT,
}

test_process_binding! {
    name: process_binding_nomode_nomod_require_shift,
    binding: Binding { trigger: KEY, mods: ModifiersState::SHIFT, action: Action::from("\x1b[1;2D"), mode: BindingMode::empty(), notmode: BindingMode::empty() },
    triggers: false,
    mode: BindingMode::empty(),
    mods: ModifiersState::empty(),
}

test_process_binding! {
    name: process_binding_nomode_controlmod,
    binding: Binding { trigger: KEY, mods: ModifiersState::CONTROL, action: Action::from("\x1b[1;5D"), mode: BindingMode::empty(), notmode: BindingMode::empty() },
    triggers: true,
    mode: BindingMode::empty(),
    mods: ModifiersState::CONTROL,
}

test_process_binding! {
    name: process_binding_nomode_nomod_require_not_appcursor,
    binding: Binding { trigger: KEY, mods: ModifiersState::empty(), action: Action::from("\x1b[D"), mode: BindingMode::empty(), notmode: BindingMode::APP_CURSOR },
    triggers: true,
    mode: BindingMode::empty(),
    mods: ModifiersState::empty(),
}

test_process_binding! {
    name: process_binding_appcursormode_nomod_require_appcursor,
    binding: Binding { trigger: KEY, mods: ModifiersState::empty(), action: Action::from("\x1bOD"), mode: BindingMode::APP_CURSOR, notmode: BindingMode::empty() },
    triggers: true,
    mode: BindingMode::APP_CURSOR,
    mods: ModifiersState::empty(),
}

test_process_binding! {
    name: process_binding_nomode_nomod_require_appcursor,
    binding: Binding { trigger: KEY, mods: ModifiersState::empty(), action: Action::from("\x1bOD"), mode: BindingMode::APP_CURSOR, notmode: BindingMode::empty() },
    triggers: false,
    mode: BindingMode::empty(),
    mods: ModifiersState::empty(),
}

test_process_binding! {
    name: process_binding_appcursormode_appkeypadmode_nomod_require_appcursor,
    binding: Binding { trigger: KEY, mods: ModifiersState::empty(), action: Action::from("\x1bOD"), mode: BindingMode::APP_CURSOR, notmode: BindingMode::empty() },
    triggers: true,
    mode: BindingMode::APP_CURSOR | BindingMode::APP_KEYPAD,
    mods: ModifiersState::empty(),
}

test_process_binding! {
    name: process_binding_fail_with_extra_mods,
    binding: Binding { trigger: KEY, mods: ModifiersState::SUPER, action: Action::from("arst"), mode: BindingMode::empty(), notmode: BindingMode::empty() },
    triggers: false,
    mode: BindingMode::empty(),
    mods: ModifiersState::ALT | ModifiersState::SUPER,
}
