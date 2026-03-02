#[cfg(not(any(feature = "zip0", feature = "zip8")))]
compile_error!("either zip0 or zip8 must be enabled");

#[cfg(feature = "zip0")]
mod zip0;
#[cfg(feature = "zip0")]
pub use zip0::ZipArchive;

#[cfg(all(not(feature = "zip0"), feature = "zip8"))]
mod zip8;
#[cfg(all(not(feature = "zip0"), feature = "zip8"))]
pub use zip8::ZipArchive;
