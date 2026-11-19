//! Library UI: search and tags, folder import, "file not found" repair, bundle
//! export / import and the local preview.
//!
//! The pieces:
//!
//! * [`logic`]: pure presentation rules (query building, tag chips and
//!   autocomplete, row building, scan summaries, path-prefix previews, bundle
//!   error texts). Unit-tested without a window.
//! * `state`: what the panel remembers between frames ([`LibraryUi`]).
//! * `tasks`: background work. Native file dialogs, folder scans, file-system
//!   checks and bundle jobs never run inside a frame; they answer through a
//!   channel that [`poll`] drains once per frame.
//! * `ops`: what the UI does to the app (open dialogs, apply their results to
//!   the library, preview, loading queue).
//! * `toolbar` / `dialogs`: the drawing.
//!
//! The sound list ([`crate::gui::sound_list`]) draws the toolbar with
//! [`library_toolbar`], builds its rows with `SoundSharedApp::list_rows` and
//! shows the dialogs through [`show_library_dialogs`].
//!
//! # Preview
//!
//! The headphones button plays a sound through
//! [`AudioEngine::play_preview`](crate::audio::AudioEngine::play_preview): a
//! local-only mixer track that is heard on this computer and never reaches the
//! players' stream. A second click on the same sound stops it.

mod dialogs;
pub mod logic;
mod ops;
mod state;
mod tasks;
mod toolbar;

pub use dialogs::show as show_library_dialogs;
pub use ops::{poll, stop_preview};
pub use state::{
    AddFolder, AddFolderPhase, Export, ExportPhase, Import, ImportPhase, ImportPreview, LibDialog,
    LibraryUi, PreviewTrack, Remap, TagEdit, Toast,
};
pub use tasks::{LibEvent, Tasks};
pub use toolbar::{
    library_toolbar, missing_row, paint_badge, paint_headphones, paint_warning, preview_button,
    MissingAction,
};
