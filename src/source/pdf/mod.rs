// both backends render the first page of a PDF to a
// framebuffer. cargo features are additive, so a build can
// carry both; pdfium takes precedence when it is enabled
#[cfg(all(feature = "pdf", not(feature = "pdf-pdfium")))]
mod via_hayro;
#[cfg(feature = "pdf-pdfium")]
mod via_pdfium;

#[cfg(all(feature = "pdf", not(feature = "pdf-pdfium")))]
pub use via_hayro::load;
#[cfg(feature = "pdf-pdfium")]
pub use via_pdfium::load;
