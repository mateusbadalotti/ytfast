//! The macOS menu bar: the app menu's Settings, and File, Edit, View,
//! Playback, Window and Help.
//!
//! The items call one handler object, which queues a `MenuCommand` and wakes
//! the window; the app turns the queue into actions at the top of a frame.

use std::cell::OnceCell;
use std::sync::Mutex;

use objc2::rc::Retained;
use objc2::runtime::{NSObject, Sel};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSApplication, NSEventModifierFlags, NSMenu, NSMenuItem};
use objc2_foundation::{NSString, ns_string};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuCommand {
    PlayPause,
    Next,
    Previous,
    SeekForward,
    SeekBackward,
    ToggleShuffle,
    CycleRepeat,
    Like,
    VolumeUp,
    VolumeDown,
    ToggleMute,
    Back,
    Forward,
    Home,
    Search,
    Library,
    LikedMusic,
    Queue,
    Lyrics,
    Settings,
    OpenRepo,
    Cut,
    Copy,
    Paste,
    SelectAll,
}

static COMMANDS: Mutex<Vec<MenuCommand>> = Mutex::new(Vec::new());
static WAKER: Mutex<Option<Box<dyn Fn() + Send + Sync>>> = Mutex::new(None);

thread_local! {
    /// The object the menu items call. Only the main thread touches it.
    static HANDLER: OnceCell<Retained<MenuHandler>> = const { OnceCell::new() };
}

/// How a menu pick wakes the window, which may be idle.
pub fn set_waker(wake: impl Fn() + Send + Sync + 'static) {
    if let Ok(mut waker) = WAKER.lock() {
        *waker = Some(Box::new(wake));
    }
}

fn push(command: MenuCommand) {
    if let Ok(mut commands) = COMMANDS.lock() {
        commands.push(command);
    }
    if let Ok(waker) = WAKER.lock()
        && let Some(wake) = waker.as_ref()
    {
        wake();
    }
}

/// The picks since the last call, oldest first.
pub fn drain() -> Vec<MenuCommand> {
    COMMANDS
        .lock()
        .map(|mut commands| std::mem::take(&mut *commands))
        .unwrap_or_default()
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "YtfastMenuHandler"]
    struct MenuHandler;

    impl MenuHandler {
        #[unsafe(method(playPause:))]
        fn play_pause(&self, _sender: &NSObject) {
            push(MenuCommand::PlayPause);
        }

        #[unsafe(method(nextTrack:))]
        fn next_track(&self, _sender: &NSObject) {
            push(MenuCommand::Next);
        }

        #[unsafe(method(previousTrack:))]
        fn previous_track(&self, _sender: &NSObject) {
            push(MenuCommand::Previous);
        }

        #[unsafe(method(seekForward:))]
        fn seek_forward(&self, _sender: &NSObject) {
            push(MenuCommand::SeekForward);
        }

        #[unsafe(method(seekBackward:))]
        fn seek_backward(&self, _sender: &NSObject) {
            push(MenuCommand::SeekBackward);
        }

        #[unsafe(method(toggleShuffle:))]
        fn toggle_shuffle(&self, _sender: &NSObject) {
            push(MenuCommand::ToggleShuffle);
        }

        #[unsafe(method(cycleRepeat:))]
        fn cycle_repeat(&self, _sender: &NSObject) {
            push(MenuCommand::CycleRepeat);
        }

        #[unsafe(method(likeTrack:))]
        fn like_track(&self, _sender: &NSObject) {
            push(MenuCommand::Like);
        }

        #[unsafe(method(volumeUp:))]
        fn volume_up(&self, _sender: &NSObject) {
            push(MenuCommand::VolumeUp);
        }

        #[unsafe(method(volumeDown:))]
        fn volume_down(&self, _sender: &NSObject) {
            push(MenuCommand::VolumeDown);
        }

        #[unsafe(method(toggleMute:))]
        fn toggle_mute(&self, _sender: &NSObject) {
            push(MenuCommand::ToggleMute);
        }

        #[unsafe(method(goBack:))]
        fn go_back(&self, _sender: &NSObject) {
            push(MenuCommand::Back);
        }

        #[unsafe(method(goForward:))]
        fn go_forward(&self, _sender: &NSObject) {
            push(MenuCommand::Forward);
        }

        #[unsafe(method(openHome:))]
        fn open_home(&self, _sender: &NSObject) {
            push(MenuCommand::Home);
        }

        #[unsafe(method(focusSearch:))]
        fn focus_search(&self, _sender: &NSObject) {
            push(MenuCommand::Search);
        }

        #[unsafe(method(openLibrary:))]
        fn open_library(&self, _sender: &NSObject) {
            push(MenuCommand::Library);
        }

        #[unsafe(method(openLikedMusic:))]
        fn open_liked_music(&self, _sender: &NSObject) {
            push(MenuCommand::LikedMusic);
        }

        #[unsafe(method(toggleQueue:))]
        fn toggle_queue(&self, _sender: &NSObject) {
            push(MenuCommand::Queue);
        }

        #[unsafe(method(toggleLyrics:))]
        fn toggle_lyrics(&self, _sender: &NSObject) {
            push(MenuCommand::Lyrics);
        }

        #[unsafe(method(openSettings:))]
        fn open_settings(&self, _sender: &NSObject) {
            push(MenuCommand::Settings);
        }

        #[unsafe(method(openRepo:))]
        fn open_repo(&self, _sender: &NSObject) {
            push(MenuCommand::OpenRepo);
        }

        // The Edit items answer to this handler rather than to the responder
        // chain: winit's view implements none of the standard editing
        // selectors, so an item aimed there does nothing, while its key
        // equivalent still takes the chord from the window. Routed through
        // egui, the same item and chord work.
        #[unsafe(method(editCut:))]
        fn edit_cut(&self, _sender: &NSObject) {
            push(MenuCommand::Cut);
        }

        #[unsafe(method(editCopy:))]
        fn edit_copy(&self, _sender: &NSObject) {
            push(MenuCommand::Copy);
        }

        #[unsafe(method(editPaste:))]
        fn edit_paste(&self, _sender: &NSObject) {
            push(MenuCommand::Paste);
        }

        #[unsafe(method(editSelectAll:))]
        fn edit_select_all(&self, _sender: &NSObject) {
            push(MenuCommand::SelectAll);
        }
    }
);

/// What an item shows and does: its title, the handler's selector (or a
/// standard one for the responder chain when `ours` is false), and its key.
struct Entry {
    title: &'static str,
    action: Sel,
    key: &'static str,
    modifiers: NSEventModifierFlags,
    ours: bool,
}

const fn entry(title: &'static str, action: Sel, key: &'static str) -> Entry {
    Entry {
        title,
        action,
        key,
        modifiers: NSEventModifierFlags::Command,
        ours: true,
    }
}

fn item(mtm: MainThreadMarker, entry: &Entry, target: &NSObject) -> Retained<NSMenuItem> {
    // SAFETY: a plain item; the selector is one the target answers to, or a
    // standard one the responder chain answers to.
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            mtm.alloc(),
            &NSString::from_str(entry.title),
            Some(entry.action),
            &NSString::from_str(entry.key),
        )
    };
    item.setKeyEquivalentModifierMask(entry.modifiers);
    if entry.ours {
        // SAFETY: the target is the handler, kept alive in `HANDLER` for as
        // long as the menu bar exists.
        unsafe { item.setTarget(Some(target)) };
    }
    item
}

/// A top-level menu of `entries`; `None` draws a separator.
fn menu(
    mtm: MainThreadMarker,
    title: &str,
    entries: &[Option<Entry>],
    target: &NSObject,
) -> Retained<NSMenuItem> {
    let title = NSString::from_str(title);
    // SAFETY: an item with no action, only a submenu.
    let holder = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(mtm.alloc(), &title, None, ns_string!(""))
    };
    let menu = NSMenu::initWithTitle(mtm.alloc(), &title);
    menu.setAutoenablesItems(false);
    for entry in entries {
        match entry {
            Some(entry) => menu.addItem(&item(mtm, entry, target)),
            None => menu.addItem(&NSMenuItem::separatorItem(mtm)),
        }
    }
    holder.setSubmenu(Some(&menu));
    holder
}

const NONE: NSEventModifierFlags = NSEventModifierFlags(0);
const RIGHT: &str = "\u{F703}";
const LEFT: &str = "\u{F702}";
const UP: &str = "\u{F700}";
const DOWN: &str = "\u{F701}";

/// Adds the menus to the bar winit made. Once, on the main thread, after
/// the application has finished launching.
pub fn init() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    let Some(bar) = app.mainMenu() else {
        log::warn!("no menu bar to add to");
        return;
    };
    if HANDLER.with(|slot| slot.get().is_some()) {
        return;
    }
    // SAFETY: a plain NSObject subclass with no ivars.
    let handler: Retained<MenuHandler> = unsafe { msg_send![mtm.alloc::<MenuHandler>(), init] };
    let target: &NSObject = &handler;

    // winit's app menu opens with About and a separator; Settings goes in
    // its own group after them, as in every Mac app.
    if let Some(app_menu) = bar.itemAtIndex(0).and_then(|item| item.submenu()) {
        let settings = item(mtm, &entry("Settings…", sel!(openSettings:), ","), target);
        app_menu.insertItem_atIndex(&settings, 2);
        app_menu.insertItem_atIndex(&NSMenuItem::separatorItem(mtm), 3);
    }

    let standard = |title, action, key| Entry {
        ours: false,
        ..entry(title, action, key)
    };
    bar.addItem(&menu(
        mtm,
        "File",
        &[Some(standard("Close Window", sel!(performClose:), "w"))],
        target,
    ));
    // No Undo or Redo: egui's text fields take Cmd+Z themselves, and an item
    // holding that chord would take it from them.
    bar.addItem(&menu(
        mtm,
        "Edit",
        &[
            Some(entry("Cut", sel!(editCut:), "x")),
            Some(entry("Copy", sel!(editCopy:), "c")),
            Some(entry("Paste", sel!(editPaste:), "v")),
            Some(entry("Select All", sel!(editSelectAll:), "a")),
        ],
        target,
    ));
    bar.addItem(&menu(
        mtm,
        "View",
        &[
            Some(entry("Back", sel!(goBack:), "[")),
            Some(entry("Forward", sel!(goForward:), "]")),
            None,
            Some(Entry {
                modifiers: NSEventModifierFlags::Command | NSEventModifierFlags::Shift,
                ..entry("Home", sel!(openHome:), "h")
            }),
            Some(entry("Search", sel!(focusSearch:), "f")),
            Some(Entry {
                modifiers: NSEventModifierFlags::Command | NSEventModifierFlags::Shift,
                ..entry("Library", sel!(openLibrary:), "l")
            }),
            Some(Entry {
                modifiers: NSEventModifierFlags::Command | NSEventModifierFlags::Shift,
                ..entry("Liked Music", sel!(openLikedMusic:), "k")
            }),
            None,
            Some(entry("Up Next", sel!(toggleQueue:), "u")),
            Some(entry("Lyrics", sel!(toggleLyrics:), "y")),
            None,
            Some(Entry {
                modifiers: NSEventModifierFlags::Command | NSEventModifierFlags::Control,
                ..standard("Toggle Full Screen", sel!(toggleFullScreen:), "f")
            }),
        ],
        target,
    ));
    // Play/Pause leaves Space to the window, which plays only when no text
    // field has it. Shift+arrow seeking stays off the menu for the same
    // reason: a key equivalent fires ahead of the focused field.
    bar.addItem(&menu(
        mtm,
        "Playback",
        &[
            Some(Entry {
                modifiers: NONE,
                ..entry("Play / Pause", sel!(playPause:), "")
            }),
            Some(entry("Next", sel!(nextTrack:), RIGHT)),
            Some(entry("Previous", sel!(previousTrack:), LEFT)),
            None,
            Some(Entry {
                modifiers: NONE,
                ..entry("Seek Forward 10 s", sel!(seekForward:), "")
            }),
            Some(Entry {
                modifiers: NONE,
                ..entry("Seek Back 10 s", sel!(seekBackward:), "")
            }),
            None,
            Some(entry("Shuffle", sel!(toggleShuffle:), "s")),
            Some(entry("Repeat", sel!(cycleRepeat:), "r")),
            Some(entry("Like", sel!(likeTrack:), "l")),
            None,
            Some(entry("Volume Up", sel!(volumeUp:), UP)),
            Some(entry("Volume Down", sel!(volumeDown:), DOWN)),
            Some(Entry {
                modifiers: NSEventModifierFlags::Command | NSEventModifierFlags::Option,
                ..entry("Mute", sel!(toggleMute:), DOWN)
            }),
        ],
        target,
    ));
    let window = menu(
        mtm,
        "Window",
        &[
            Some(standard("Minimize", sel!(performMiniaturize:), "m")),
            Some(Entry {
                modifiers: NONE,
                ..standard("Zoom", sel!(performZoom:), "")
            }),
            None,
            Some(Entry {
                modifiers: NONE,
                ..standard("Bring All to Front", sel!(arrangeInFront:), "")
            }),
        ],
        target,
    );
    bar.addItem(&window);
    // The system lists the open windows under the Window menu.
    app.setWindowsMenu(window.submenu().as_deref());
    let help = menu(
        mtm,
        "Help",
        &[Some(Entry {
            modifiers: NONE,
            ..entry("ytfast on GitHub", sel!(openRepo:), "")
        })],
        target,
    );
    bar.addItem(&help);
    // The system adds its menu search to the Help menu.
    app.setHelpMenu(help.submenu().as_deref());

    // NSMenuItem does not retain its target, which has to answer for as long
    // as the menu bar exists: one object, kept for the main thread's life.
    HANDLER.with(|slot| {
        let _ = slot.set(handler);
    });
}
