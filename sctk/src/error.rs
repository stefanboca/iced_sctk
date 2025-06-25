use crate::{futures::futures, graphics};

/// An error that occurred while running an application.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The futures executor could not be created.
    #[error("the futures executor could not be created")]
    ExecutorCreationFailed(futures::io::Error),

    /// The application graphics context could not be created.
    #[error("the application graphics context could not be created")]
    GraphicsCreationFailed(graphics::Error),
}

impl From<graphics::Error> for Error {
    fn from(error: graphics::Error) -> Error {
        Error::GraphicsCreationFailed(error)
    }
}
