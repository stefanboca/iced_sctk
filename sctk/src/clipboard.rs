use crate::core::clipboard::Kind;

pub struct Clipboard {}

impl Clipboard {
    pub fn unconnected() -> Self {
        Self {}
    }
}

impl crate::core::Clipboard for Clipboard {
    fn read(&self, kind: Kind) -> Option<String> {
        todo!("Clipboard::read")
    }

    fn write(&mut self, kind: Kind, contents: String) {
        todo!("Clipboard::write")
    }
}
