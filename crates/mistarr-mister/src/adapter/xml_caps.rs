//! One capped, depth-tracked way to read an XML event, shared by the MRA and
//! `romsets.xml` readers so their limits cannot drift apart.
//! See `docs/PLATFORMS.md` "MRA catalogue" and "Neo Geo `romsets.xml`".

use std::io::BufRead;

use mistarr_core::dat::{MAX_DEPTH, MAX_EVENT_BYTES};
use mistarr_core::xml::Capped;
use quick_xml::events::Event;
use quick_xml::Reader;

use crate::Error;

/// True element nesting depth: one level added per `Event::Start`, one removed per
/// `Event::End`, independent of any name a caller's own recovery logic gives an end
/// tag. This mirrors what `quick_xml` itself keeps in its opened-name buffer under
/// `check_end_names = false`, so bounding this bounds that buffer's size too.
#[derive(Debug, Default)]
pub(super) struct Depth(usize);

impl Depth {
    /// Updates the count for `event`, failing once the true nesting passes [`MAX_DEPTH`].
    pub(super) fn track(&mut self, event: &Event<'_>, position: u64) -> Result<(), Error> {
        match event {
            Event::Start(_) => {
                self.0 += 1;
                if self.0 > MAX_DEPTH {
                    return Err(Error::XmlTooDeep { position });
                }
            }
            Event::End(_) => self.0 = self.0.saturating_sub(1),
            _ => {}
        }
        Ok(())
    }
}

/// The next event, capped at [`MAX_EVENT_BYTES`] before it is buffered.
pub(super) fn read_capped<'b, R: BufRead>(
    reader: &mut Reader<Capped<R>>,
    buf: &'b mut Vec<u8>,
) -> quick_xml::Result<Event<'b>> {
    buf.clear();
    reader.get_mut().arm(MAX_EVENT_BYTES);
    reader.read_event_into(buf)
}
