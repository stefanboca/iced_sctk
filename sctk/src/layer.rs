use std::{ffi::c_void, marker::PhantomData, ptr::NonNull};

use iced_debug::core::Size;
use iced_program::runtime::window::raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle,
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle,
    WaylandWindowHandle, WindowHandle,
};
use smithay_client_toolkit::{
    reexports::client::{protocol::wl_display::WlDisplay, Proxy},
    shell::{wlr_layer::LayerSurface, WaylandSurface},
};

use crate::program::Program;

pub struct Layer<P> {
    display: WlDisplay,
    layer_surface: LayerSurface,
    size: Option<Size>,
    phantom: PhantomData<P>,
}

impl<P> Layer<P>
where
    P: Program + 'static,
{
    pub fn new(display: WlDisplay, layer_surface: LayerSurface) -> Self {
        Self {
            display,
            layer_surface,
            size: None,
            phantom: PhantomData,
        }
    }

    pub fn configure(&mut self, size: Size) {
        self.size = Some(size);
        self.layer_surface.commit();
    }
}

impl<P> HasDisplayHandle for Layer<P>
where
    P: Program + 'static,
{
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        let display = self.display.id().as_ptr() as *mut c_void;

        let c_ptr = NonNull::new(display).ok_or(HandleError::Unavailable)?;
        let handle = WaylandDisplayHandle::new(c_ptr);
        let raw_handle = RawDisplayHandle::Wayland(handle);
        #[allow(unsafe_code)]
        Ok(unsafe { DisplayHandle::borrow_raw(raw_handle) })
    }
}

impl<P> HasWindowHandle for Layer<P>
where
    P: Program + 'static,
{
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let surface =
            self.layer_surface.wl_surface().id().as_ptr() as *mut c_void;
        let c_ptr = NonNull::new(surface).ok_or(HandleError::Unavailable)?;
        let handle = WaylandWindowHandle::new(c_ptr);
        let raw_handle = RawWindowHandle::Wayland(handle);
        #[allow(unsafe_code)]
        Ok(unsafe { WindowHandle::borrow_raw(raw_handle) })
    }
}
