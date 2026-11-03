//! Type into whatever window has focus, with XTEST.
//!
//! There is no `xdotool` on this machine, and a window's key handling cannot
//! be tested by reasoning about it — the GPU window shipped without a key
//! filter at all, and no amount of reading the mapping code would have shown
//! that, because the mapping code was correct.
//!
//! usage: xsend 'C-a,%'   (comma-separated; `C-x` means Ctrl+x)

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{self, ConnectionExt as _};
use x11rb::protocol::xtest::ConnectionExt as _;

const KEY_PRESS: u8 = 2;
const KEY_RELEASE: u8 = 3;

fn main() {
    let Some(spec) = std::env::args().nth(1) else {
        eprintln!("usage: xsend 'C-a,%'");
        std::process::exit(2);
    };
    let (conn, screen_num) = x11rb::connect(None).expect("connect to X");

    // With no window manager running there is nothing to give a window the
    // input focus, and XTEST delivers to whatever has it — so without this
    // every keystroke goes to the root and the test silently proves nothing.
    let root = conn.setup().roots[screen_num].root;
    if let Some(tree) = conn.query_tree(root).ok().and_then(|c| c.reply().ok()) {
        for child in tree.children {
            let viewable = conn
                .get_window_attributes(child)
                .ok()
                .and_then(|c| c.reply().ok())
                .map(|a| a.map_state == xproto::MapState::VIEWABLE)
                .unwrap_or(false);
            if viewable {
                let _ =
                    conn.set_input_focus(xproto::InputFocus::PARENT, child, x11rb::CURRENT_TIME);
                let _ = conn.flush();
                eprintln!("xsend: focused window 0x{child:x}");
                break;
            }
        }
    }

    let setup = conn.setup();
    let (min, max) = (setup.min_keycode, setup.max_keycode);
    let mapping = conn
        .get_keyboard_mapping(min, max - min + 1)
        .expect("request mapping")
        .reply()
        .expect("mapping");
    let per = mapping.keysyms_per_keycode as usize;

    // Find a keycode (and whether shift is needed) for a keysym.
    let find = |keysym: u32| -> Option<(u8, bool)> {
        for (i, chunk) in mapping.keysyms.chunks(per).enumerate() {
            for (col, &ks) in chunk.iter().enumerate().take(2) {
                if ks == keysym {
                    return Some((min + i as u8, col == 1));
                }
            }
        }
        None
    };

    let shift = find(0xffe1).map(|(k, _)| k).expect("no shift key");
    let ctrl = find(0xffe3).map(|(k, _)| k).expect("no control key");

    for item in spec.split(',') {
        let (want_ctrl, ch) = match item.strip_prefix("C-") {
            Some(rest) => (true, rest),
            None => (false, item),
        };
        // Named keys first, then Latin-1, whose keysyms equal their code point.
        let keysym = match ch {
            "Return" | "Enter" => 0xff0d,
            "Escape" | "Esc" => 0xff1b,
            "BackSpace" => 0xff08,
            "space" => 0x20,
            "Prior" | "PageUp" => 0xff55,
            "Next" | "PageDown" => 0xff56,
            "Home" => 0xff50,
            "End" => 0xff57,
            _ => match ch.chars().next() {
                Some(c) => c as u32,
                None => continue,
            },
        };
        let Some((code, want_shift)) = find(keysym) else {
            eprintln!("xsend: no key for {ch:?}");
            continue;
        };

        if want_ctrl {
            fake(&conn, KEY_PRESS, ctrl);
        }
        if want_shift {
            fake(&conn, KEY_PRESS, shift);
        }
        fake(&conn, KEY_PRESS, code);
        fake(&conn, KEY_RELEASE, code);
        if want_shift {
            fake(&conn, KEY_RELEASE, shift);
        }
        if want_ctrl {
            fake(&conn, KEY_RELEASE, ctrl);
        }
        conn.flush().expect("flush");
        std::thread::sleep(std::time::Duration::from_millis(120));
    }
}

fn fake(conn: &impl Connection, kind: u8, code: u8) {
    conn.xtest_fake_input(kind, code, 0, x11rb::NONE, 0, 0, 0).expect("fake input");
}
