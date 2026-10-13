//! better-video's native engine: everything needed to go from screen to GIF
//! without ffmpeg.
//!
//! - [`capture`] grabs a screen region (Windows GDI), cursor included.
//! - [`raw`] is the intermediate recording: only the 32×32 tiles that changed
//!   since the previous frame are stored, LZ4-compressed, so a mostly static
//!   screen costs almost nothing to record.
//! - [`recorder`] runs capture → raw on a background thread.
//! - [`quant`] builds a 255-colour palette (exact when the clip has few
//!   colours, median cut otherwise) and maps pixels to it.
//! - [`gif`] + [`lzw`] write the GIF: only the changed rectangle of each
//!   frame is encoded, unchanged pixels inside it are transparent, and
//!   identical frames are merged into one longer frame.

pub mod capture;
pub mod gif;
pub mod lzw;
pub mod mp4;
pub mod quant;
pub mod raw;
pub mod recorder;

pub use gif::GifEncoder;
pub use mp4::Mp4Encoder;
pub use quant::{Palette, PaletteBuilder};
pub use raw::{RawReader, RawWriter};
pub use recorder::{Recorder, RecorderStats};
