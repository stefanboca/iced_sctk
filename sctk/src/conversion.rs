use crate::core;
use smithay_client_toolkit as sctk;

pub mod mouse {
    use super::{core, sctk};

    pub fn button(button: u32) -> core::mouse::Button {
        use core::mouse::Button;
        use sctk::seat::pointer::*;

        match button {
            BTN_LEFT => Button::Left,
            BTN_RIGHT => Button::Right,
            BTN_MIDDLE => Button::Middle,
            BTN_BACK => Button::Back,
            BTN_FORWARD => Button::Forward,
            _ => Button::Other(button as u16),
        }
    }
}

pub mod keyboard {
    use iced_debug::core::SmolStr;

    use super::{core, sctk};

    pub fn modifiers(
        modifiers: sctk::seat::keyboard::Modifiers,
    ) -> core::keyboard::Modifiers {
        let mut m = core::keyboard::Modifiers::empty();

        if modifiers.shift {
            m |= core::keyboard::Modifiers::SHIFT;
        }
        if modifiers.ctrl {
            m |= core::keyboard::Modifiers::CTRL;
        }
        if modifiers.alt {
            m |= core::keyboard::Modifiers::ALT;
        }
        if modifiers.logo {
            m |= core::keyboard::Modifiers::LOGO;
        }

        m
    }

    pub fn key(keysym: sctk::seat::keyboard::Keysym) -> core::keyboard::Key {
        use core::keyboard::{key::Named as N, Key as IK};
        use sctk::seat::keyboard::Keysym as SK;
        IK::Named(match keysym {
            SK::Alt_L | SK::Alt_R => N::Alt,
            SK::Caps_Lock => N::CapsLock,
            SK::Control_L | SK::Control_R => N::Control,
            SK::XF86_Fn => N::Fn,
            SK::Num_Lock => N::NumLock,
            SK::Scroll_Lock => N::ScrollLock,
            SK::Shift_L | SK::Shift_R => N::Shift,
            SK::Meta_L | SK::Meta_R => N::Meta,
            SK::Hyper_L | SK::Hyper_R => N::Hyper,
            SK::Super_L | SK::Super_R => N::Super,
            SK::KP_Enter | SK::ISO_Enter => N::Enter,
            SK::Tab => N::Tab,
            SK::KP_Space => N::Space,
            SK::Down => N::ArrowDown,
            SK::Left => N::ArrowLeft,
            SK::Right => N::ArrowRight,
            SK::Up => N::ArrowUp,
            SK::End => N::End,
            SK::Home => N::Home,
            SK::Page_Down => N::PageDown,
            SK::Page_Up => N::PageUp,
            SK::BackSpace => N::Backspace,
            SK::Clear => N::Clear,
            SK::Delete => N::Delete,
            SK::Insert => N::Insert,
            SK::Escape => N::Escape,
            // NOTE: only a small subset of named keys are currently handled. add as needed.
            _ => {
                if let Some(char) = keysym.key_char() {
                    let mut b = [0; 4];
                    return IK::Character(SmolStr::new_inline(
                        char.encode_utf8(&mut b),
                    ));
                }

                return IK::Unidentified;
            }
        })
    }

    pub fn code(
        keysym: sctk::seat::keyboard::Keysym,
        raw_code: u32,
    ) -> core::keyboard::key::Physical {
        use core::keyboard::key::{Code as C, NativeCode, Physical};
        use sctk::seat::keyboard::Keysym as SK;
        Physical::Code(match keysym {
            SK::Caps_Lock => C::CapsLock,
            SK::XF86_Fn => C::Fn,
            SK::Num_Lock => C::NumLock,
            SK::Scroll_Lock => C::ScrollLock,
            SK::Meta_L | SK::Meta_R => C::Meta,
            SK::Hyper_L | SK::Hyper_R => C::Hyper,
            SK::KP_Enter | SK::ISO_Enter => C::Enter,
            SK::Tab => C::Tab,
            SK::KP_Space => C::Space,
            SK::Down => C::ArrowDown,
            SK::Left => C::ArrowLeft,
            SK::Right => C::ArrowRight,
            SK::Up => C::ArrowUp,
            SK::End => C::End,
            SK::Home => C::Home,
            SK::Page_Down => C::PageDown,
            SK::Page_Up => C::PageUp,
            SK::BackSpace => C::Backspace,
            SK::Delete => C::Delete,
            SK::Insert => C::Insert,
            SK::Escape => C::Escape,
            // NOTE: only a small subset of codes are currently handled. add as needed.
            _ => return Physical::Unidentified(NativeCode::Xkb(raw_code)),
        })
    }

    pub fn location(
        keysym: sctk::seat::keyboard::Keysym,
    ) -> core::keyboard::Location {
        use core::keyboard::Location;
        use sctk::seat::keyboard::Keysym as SK;

        if keysym.is_keypad_key() | keysym.is_private_keypad_key() {
            Location::Numpad
        } else if matches!(
            keysym,
            SK::Alt_L
                | SK::Control_R
                | SK::Shift_L
                | SK::Meta_L
                | SK::Hyper_L
                | SK::Super_L
        ) {
            Location::Left
        } else if matches!(
            keysym,
            SK::Alt_R
                | SK::Shift_R
                | SK::Control_R
                | SK::Meta_R
                | SK::Hyper_R
                | SK::Super_R
        ) {
            Location::Right
        } else {
            Location::Standard
        }
    }
}
