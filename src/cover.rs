//! A still of an embedded native view, standing in for it while the
//! toolkit draws over it.
//!
//! A native view — a web view, a video player — is composited over the
//! GPUI window, so a palette or a list drawn where the view is does not
//! show. A host covers the view instead: it asks the view for a still,
//! shows the still in the view's place, hides the view, and draws over the
//! still; when the last thing drawn over it closes, the view comes back.
//!
//! How a still is taken is the host's business, and the view's: this only
//! keeps the bookkeeping straight. A request is a token, and a still that
//! arrives for an earlier token than the latest — asked for before a
//! switch, or before the overlay was opened again — is dropped, so what
//! shows is never a still of a page that is no longer in front.
//!
//! ```ignore
//! let request = self.cover.request();
//! take_still_of(&view, move |image| sender.send(Event::Still(request, image)));
//! // Later, on the event:
//! if self.cover.deliver(request, image) && self.cover.still().is_some() {
//!     view.set_visible(false);
//! }
//! // In render, in the view's place:
//! .children(self.cover.backdrop())
//! ```

use std::sync::Arc;

use gpui::{
    AnyElement, App, Image, ImageFormat, ImageSource, ImgResourceLoader, ObjectFit, Window, img,
    prelude::*,
};

/// One request for a still: handed to whoever takes it, and handed back
/// with the still, so an answer to an earlier request can be told from the
/// latest. A counter rather than the clock, which can hand two requests
/// made in the same tick the same time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Request(u64);

/// The bookkeeping of one cover: which request is current, and the still
/// that answered it.
#[derive(Default)]
pub struct Cover {
    /// How many requests have been made: the next token.
    made: u64,
    /// The request not yet answered, if any.
    request: Option<Request>,
    still: Option<ImageSource>,
    /// Stills let go of, until a window frees what they took on the GPU:
    /// see [`Self::release`].
    let_go: Vec<ImageSource>,
}

impl Cover {
    pub fn new() -> Self {
        Self::default()
    }

    /// Asks for a still: the token the host passes along with its request,
    /// so the answer can be told from a stale one. An earlier request still
    /// pending is superseded; a still already showing is kept until the
    /// new one arrives, so nothing flashes blank.
    pub fn request(&mut self) -> Request {
        self.made += 1;
        let request = Request(self.made);
        self.request = Some(request);
        request
    }

    /// Whether a request has been made and not yet answered.
    pub fn pending(&self) -> bool {
        self.request.is_some()
    }

    /// A still arrived for `request`: anything GPUI draws as an image, a
    /// decoded `RenderImage` as much as a path. Accepted only if it answers
    /// the latest request, in which case it replaces what was showing — or,
    /// if the view could not be captured, clears it. Returns whether it was
    /// accepted.
    pub fn deliver(&mut self, request: Request, still: Option<impl Into<ImageSource>>) -> bool {
        if self.request != Some(request) {
            return false;
        }
        self.request = None;
        self.let_go
            .extend(std::mem::replace(&mut self.still, still.map(Into::into)));
        true
    }

    /// [`Self::deliver`] for an encoded image, as a view hands one over.
    pub fn deliver_bytes(
        &mut self,
        request: Request,
        bytes: Option<Vec<u8>>,
        format: ImageFormat,
    ) -> bool {
        self.deliver(
            request,
            bytes.map(|bytes| Arc::new(Image::from_bytes(format, bytes))),
        )
    }

    /// The still showing, if one has arrived.
    pub fn still(&self) -> Option<&ImageSource> {
        self.still.as_ref()
    }

    /// The still in the view's place, filling whatever it is put in: it is
    /// a picture of that very place, so it is stretched to it rather than
    /// fitted inside it.
    pub fn backdrop(&self) -> Option<AnyElement> {
        let still = self.still.clone()?;
        Some(
            img(still)
                .object_fit(ObjectFit::Fill)
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .into_any_element(),
        )
    }

    /// Forgets the still and any pending request: the overlay has closed
    /// and the view is back.
    pub fn clear(&mut self) {
        self.request = None;
        self.let_go.extend(self.still.take());
    }

    /// Frees what the stills let go of since the last call held on the
    /// GPU. A still is a window's worth of pixels, and GPUI keeps an
    /// image's texture until told otherwise, so a host that covers its
    /// view many times would otherwise hold every still it ever showed.
    /// Call it while drawing, where there is a window to free them from;
    /// a still still showing is never touched.
    pub fn release(&mut self, window: &mut Window, cx: &mut App) {
        let mut freed: Vec<ImageSource> = Vec::new();
        for still in std::mem::take(&mut self.let_go) {
            // GPUI names an encoded image by its bytes and a path by the
            // path, so a still let go of can be the very image showing (an
            // unchanged page captured again), or one already freed this
            // call. Freeing it would blank what shows, or decode it again
            // only to free it.
            if self
                .still
                .iter()
                .chain(&freed)
                .any(|kept| same_image(kept, &still))
            {
                continue;
            }
            match &still {
                ImageSource::Render(image) => cx.drop_image(image.clone(), Some(window)),
                // An encoded still is decoded into GPUI's asset cache, and
                // what the GPU holds is the decoded image's: forgetting the
                // cache entry alone would leave the texture behind.
                ImageSource::Image(image) => {
                    if let Some(decoded) = image.clone().get_render_image(window, cx) {
                        cx.drop_image(decoded, Some(window));
                    }
                    still.remove_asset(cx);
                }
                // A path or a URI is decoded into the cache the same way.
                ImageSource::Resource(resource) => {
                    if let Some(Ok(decoded)) = window.get_asset::<ImgResourceLoader>(resource, cx) {
                        cx.drop_image(decoded, Some(window));
                    }
                    still.remove_asset(cx);
                }
                ImageSource::Custom(_) => {}
            }
            freed.push(still);
        }
    }

    /// How many stills wait for [`Self::release`].
    #[cfg(test)]
    fn let_go(&self) -> usize {
        self.let_go.len()
    }
}

/// Whether two stills come to the same image on the GPU: GPUI caches an
/// encoded image under its bytes' hash and a resource under its location,
/// so two sources that are not the same value can still share a texture.
fn same_image(a: &ImageSource, b: &ImageSource) -> bool {
    match (a, b) {
        (ImageSource::Render(a), ImageSource::Render(b)) => a.id == b.id,
        (ImageSource::Image(a), ImageSource::Image(b)) => a.id() == b.id(),
        (ImageSource::Resource(a), ImageSource::Resource(b)) => a == b,
        (ImageSource::Custom(a), ImageSource::Custom(b)) => Arc::ptr_eq(a, b),
        _ => false,
    }
}

/// The width to ask a view for a still at, in points, so that it comes to
/// about one 1080p frame however large or dense the view: a Retina or very
/// large window would otherwise multiply the encoding work and the GPU
/// memory of an image that stands in for a moment. `None` for a view with
/// no size, or one it is unwise to size by.
pub fn snapshot_width(width: f64, height: f64, scale: f64) -> Option<f64> {
    if !width.is_finite()
        || !height.is_finite()
        || !scale.is_finite()
        || width <= 0.0
        || height <= 0.0
        || scale <= 0.0
    {
        return None;
    }
    let pixels_wide = width * scale;
    let pixels_high = height * scale;
    let ratio = (2048.0 / pixels_wide.max(pixels_high))
        .min((2_097_152.0 / (pixels_wide * pixels_high)).sqrt())
        .min(1.0);
    Some(width * ratio)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_latest_request_is_answered() {
        let mut cover = Cover::new();
        assert!(!cover.pending());
        let first = cover.request();
        let second = cover.request();
        assert_ne!(first, second);
        assert!(cover.pending());
        // Asked for before the second: no longer wanted.
        assert!(!cover.deliver(first, None::<ImageSource>));
        assert!(cover.pending());
        assert!(cover.deliver_bytes(second, Some(vec![1, 2, 3]), ImageFormat::Jpeg));
        assert!(!cover.pending());
        assert!(cover.still().is_some());
        // Answered twice: the second answer is stale too.
        assert!(!cover.deliver(second, None::<ImageSource>));
        assert!(cover.still().is_some());
        // A capture that failed clears what showed, which waits to be freed.
        let third = cover.request();
        assert!(cover.deliver(third, None::<ImageSource>));
        assert!(cover.still().is_none());
        assert_eq!(cover.let_go(), 1);
        cover.clear();
        assert!(!cover.pending());
        assert_eq!(cover.let_go(), 1);
    }

    #[test]
    fn a_still_replaced_or_cleared_waits_to_be_freed() {
        let mut cover = Cover::new();
        let request = cover.request();
        assert!(cover.deliver_bytes(request, Some(vec![1]), ImageFormat::Png));
        let request = cover.request();
        assert!(cover.deliver_bytes(request, Some(vec![2]), ImageFormat::Png));
        assert_eq!(cover.let_go(), 1);
        cover.clear();
        assert_eq!(cover.let_go(), 2);
        assert!(cover.still().is_none());
    }

    #[test]
    fn an_identical_capture_is_the_same_image() {
        let still = |bytes: Vec<u8>| {
            ImageSource::from(Arc::new(Image::from_bytes(ImageFormat::Png, bytes)))
        };
        // The page did not change between captures: separate values, one
        // cache entry, and freeing the old would free what shows.
        assert!(same_image(&still(vec![1, 2]), &still(vec![1, 2])));
        assert!(!same_image(&still(vec![1, 2]), &still(vec![3])));
        let path = |name: &str| ImageSource::from(std::path::PathBuf::from(name));
        assert!(same_image(&path("/tmp/a.png"), &path("/tmp/a.png")));
        assert!(!same_image(&path("/tmp/a.png"), &path("/tmp/b.png")));
        assert!(!same_image(&path("/tmp/a.png"), &still(vec![1])));
    }

    #[test]
    fn bounds_retina_and_large_stills_without_upscaling() {
        for (width, height, scale) in [
            (900.0, 600.0, 1.0),
            (900.0, 600.0, 2.0),
            (2560.0, 1440.0, 2.0),
            (900.0, 2400.0, 2.0),
        ] {
            let snapshot = snapshot_width(width, height, scale).unwrap();
            let pixels_wide = snapshot * scale;
            let pixels_high = pixels_wide * height / width;
            assert!(snapshot <= width);
            assert!(pixels_wide.max(pixels_high) <= 2048.0001);
            assert!(pixels_wide * pixels_high <= 2_097_152.001);
        }
        assert_eq!(snapshot_width(900.0, 600.0, 1.0), Some(900.0));
        assert_eq!(snapshot_width(800.0, 600.0, 2.0), Some(800.0));
        assert_eq!(snapshot_width(0.0, 600.0, 2.0), None);
        assert_eq!(snapshot_width(900.0, f64::NAN, 2.0), None);
    }
}
