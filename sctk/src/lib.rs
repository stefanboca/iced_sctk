//! A windowing shell for Iced, on top of [`smithay-client-toolkit`].

use std::time::Instant;

use iced_debug::{
    core::{renderer, widget::operation, window::RedrawRequest, SmolStr},
    futures::futures::channel::oneshot,
};
pub use iced_program as program;
pub use program::{core, graphics, runtime};
pub use runtime::{debug, futures};
#[cfg(feature = "system")]
pub mod system;

mod clipboard;
mod conversion;
mod error;
mod proxy;
mod window;

use iced_debug::core::layer_shell;
use iced_program::runtime::{user_interface, UserInterface};
use rustc_hash::FxHashMap;
use sctk::{
    compositor::{CompositorHandler, CompositorState},
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{
            timer::{TimeoutAction, Timer},
            Dispatcher, EventLoop, LoopHandle, LoopSignal, RegistrationToken,
        },
        calloop_wayland_source::WaylandSource,
        client::{
            delegate_noop,
            globals::registry_queue_init,
            protocol::{
                wl_display, wl_keyboard, wl_output, wl_pointer, wl_seat, wl_surface, wl_touch,
            },
            Connection, Proxy, QueueHandle,
        },
        protocols::wp::text_input::zv3::client::zwp_text_input_manager_v3::ZwpTextInputManagerV3,
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        keyboard::KeyboardHandler,
        pointer::{PointerData, PointerHandler, ThemedPointer},
        touch::TouchHandler,
        SeatHandler, SeatState,
    },
    shell::{
        wlr_layer::{self, LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure},
        WaylandSurface,
    },
    shm::{Shm, ShmHandler},
};

pub use crate::error::Error;
use crate::{
    clipboard::Clipboard,
    core::{theme, Settings},
    futures::{subscription, Executor, Runtime},
    graphics::{compositor, Compositor},
    program::Program,
    proxy::ProxySink,
    runtime::Action,
    window::{RawWindow, WindowManager},
};

/// Runs a [`Program`] with the provided settings.
pub fn run<P>(
    program: P,
    settings: Settings,
    window_settings: Option<core::window::Settings>,
) -> Result<(), Error>
where
    P: Program + 'static,
    P::Theme: theme::Base,
{
    let boot_span = debug::boot();

    let mut event_loop: EventLoop<'_, State<P>> =
        EventLoop::try_new().expect("initializing the event loop should succeed");
    let loop_handle = event_loop.handle();

    let (proxy_sink, proxy_source) = crate::proxy::new();
    let mut runtime = Runtime::new(
        P::Executor::new().map_err(Error::ExecutorCreationFailed)?,
        proxy_sink,
    );

    let (program, task) = runtime.enter(|| program::Instance::new(program));
    let is_daemon = window_settings.is_none();

    let task = if let Some(_window_settings) = window_settings {
        let mut task = Some(task);

        // HACK: fix after implementing normal windows
        // let (_id, open) = runtime::window::open(window_settings);

        let (_id, open) = runtime::layer_shell::open(layer_shell::Settings {
            layer: core::layer_shell::Layer::Top,
            namespace: None,
            size: core::Size {
                width: 400,
                height: 400,
            },
            anchor: core::layer_shell::Anchor::TOP,
            exclusive_zone: 0,
            margin: core::Padding {
                top: 200,
                right: 0,
                bottom: 0,
                left: 0,
            },
            keyboard_interactivity: core::layer_shell::KeyboardInteractivity::OnDemand,
            output: None,
        });

        open.then(move |_| task.take().unwrap_or(runtime::Task::none()))
    } else {
        task
    };

    if let Some(stream) = runtime::task::into_stream(task) {
        runtime.run(stream);
    }

    runtime.track(subscription::into_recipes(
        runtime.enter(|| program.subscription().map(Action::Output)),
    ));

    let conn = Connection::connect_to_env().unwrap();
    let display = conn.display();
    let (globals, event_queue) = registry_queue_init(&conn).unwrap();
    let qh = event_queue.handle();

    let loop_timer_dispatcher =
        Dispatcher::new(Timer::immediate(), |now, _, state: &mut State<P>| {
            state.on_timer_wake(now)
        });

    let loop_timer_handle = loop_timer_dispatcher.clone();
    let loop_timer_token = loop_handle
        .register_dispatcher(loop_timer_dispatcher)
        .unwrap();

    let _ = loop_handle
        .insert_source(proxy_source, |action, (), state| {
            state.run_action(action);
        })
        .unwrap();

    let _ = WaylandSource::new(conn.clone(), event_queue)
        .insert(loop_handle.clone())
        .unwrap();

    let mut state = State {
        conn,
        display,
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        seat_state: SeatState::new(&globals, &qh),
        wl_compositor: CompositorState::bind(&globals, &qh).unwrap(),
        shm: Shm::bind(&globals, &qh).unwrap(),
        layer_shell: LayerShell::bind(&globals, &qh).unwrap(),
        text_input_manager: globals.bind(&qh, 1..=1, ()).ok(),
        qh,

        keyboards: FxHashMap::default(),
        pointers: FxHashMap::default(),
        touch: FxHashMap::default(),

        keyboard_focuses: FxHashMap::default(),
        touches: FxHashMap::default(),

        is_daemon,
        error: None,
        loop_handle,
        loop_signal: event_loop.get_signal(),
        loop_timer_handle,
        loop_timer_token,

        settings,

        runtime,
        program_wrapper: Some(
            ProgramWrapperBuilder {
                program,
                user_interfaces_builder: |_| FxHashMap::default(),
            }
            .build(),
        ),
        compositor: None,

        window_manager: WindowManager::new(),
        clipboard: Clipboard::unconnected(),
        messages: Vec::new(),
        events: Vec::new(),
        actions: 0,
        in_progress_windows: FxHashMap::default(),
    };

    boot_span.finish();

    let _ = event_loop.run(None, &mut state, |state| {
        state.about_to_wait();
    });

    state.error.map(Err).unwrap_or(Ok(()))
}

struct InProgressWindow {
    id: core::window::Id,
    raw_window: RawWindow,
    sender: oneshot::Sender<core::window::Id>,
}

#[ouroboros::self_referencing]
struct ProgramWrapper<P>
where
    P: Program + 'static,
{
    program: program::Instance<P>,
    #[borrows(program)]
    #[covariant]
    user_interfaces:
        FxHashMap<core::window::Id, UserInterface<'this, P::Message, P::Theme, P::Renderer>>,
}

struct State<P>
where
    P: Program + 'static,
{
    conn: Connection,
    qh: QueueHandle<State<P>>,
    display: wl_display::WlDisplay,
    registry_state: RegistryState,
    output_state: OutputState,
    seat_state: SeatState,
    wl_compositor: CompositorState,
    shm: Shm,
    layer_shell: LayerShell,
    text_input_manager: Option<ZwpTextInputManagerV3>,

    keyboards: FxHashMap<wl_seat::WlSeat, wl_keyboard::WlKeyboard>,
    pointers: FxHashMap<wl_seat::WlSeat, ThemedPointer>,
    touch: FxHashMap<wl_seat::WlSeat, wl_touch::WlTouch>,

    keyboard_focuses: FxHashMap<wl_keyboard::WlKeyboard, core::window::Id>,
    touches: FxHashMap<wl_touch::WlTouch, FxHashMap<i32, (core::window::Id, core::Point)>>,

    is_daemon: bool,
    error: Option<Error>,
    loop_handle: LoopHandle<'static, State<P>>,
    loop_signal: LoopSignal,
    loop_timer_handle: Dispatcher<'static, Timer, State<P>>,
    loop_timer_token: RegistrationToken,

    settings: core::Settings,

    runtime: Runtime<P::Executor, ProxySink<P::Message>, Action<P::Message>>,
    program_wrapper: Option<ProgramWrapper<P>>,
    compositor: Option<<P::Renderer as compositor::Default>::Compositor>,

    window_manager: WindowManager<P>,
    clipboard: Clipboard,
    messages: Vec<P::Message>,
    events: Vec<(core::window::Id, core::Event)>,
    actions: usize,

    in_progress_windows: FxHashMap<wl_surface::WlSurface, InProgressWindow>,
}

impl<P: Program + 'static> State<P> {
    fn open_layer(
        &mut self,
        id: core::window::Id,
        settings: core::layer_shell::Settings,
        sender: oneshot::Sender<core::window::Id>,
    ) {
        let output = match &settings.output {
            Some(name) => self.output_state.outputs().find(|output| {
                self.output_state
                    .info(output)
                    .and_then(|output| output.name)
                    .is_some_and(|n| n.eq(name))
            }),
            None => None,
        };

        let surface = self.wl_compositor.create_surface(&self.qh);

        let layer_surface = self.layer_shell.create_layer_surface(
            &self.qh,
            surface.clone(),
            match settings.layer {
                core::layer_shell::Layer::Background => wlr_layer::Layer::Background,
                core::layer_shell::Layer::Bottom => wlr_layer::Layer::Bottom,
                core::layer_shell::Layer::Top => wlr_layer::Layer::Top,
                core::layer_shell::Layer::Overlay => wlr_layer::Layer::Overlay,
            },
            settings.namespace,
            output.as_ref(),
        );

        layer_surface.set_size(settings.size.width, settings.size.height);
        layer_surface.set_anchor(wlr_layer::Anchor::from_bits(settings.anchor.bits()).unwrap());
        layer_surface.set_exclusive_zone(settings.exclusive_zone);
        layer_surface.set_margin(
            settings.margin.top,
            settings.margin.right,
            settings.margin.bottom,
            settings.margin.left,
        );
        layer_surface.set_keyboard_interactivity(match settings.keyboard_interactivity {
            core::layer_shell::KeyboardInteractivity::None => {
                wlr_layer::KeyboardInteractivity::None
            }
            core::layer_shell::KeyboardInteractivity::Exclusive => {
                wlr_layer::KeyboardInteractivity::Exclusive
            }
            core::layer_shell::KeyboardInteractivity::OnDemand => {
                wlr_layer::KeyboardInteractivity::OnDemand
            }
        });

        layer_surface.commit();

        let _ = self.in_progress_windows.insert(
            surface,
            InProgressWindow {
                id,
                raw_window: RawWindow::Layer(self.display.clone(), layer_surface),
                sender,
            },
        );
    }

    fn close_window(&mut self, id: core::window::Id) {
        if !self.is_daemon && self.in_progress_windows.is_empty() && self.window_manager.is_empty()
        {
            self.exit(None);
            return;
        }

        // NOTE: when implementing normal windows, remember to properly handle exit_on_close_request = false

        if let Some(program_wrapper) = self.program_wrapper.as_mut() {
            let _ = program_wrapper
                .with_user_interfaces_mut(|user_interfaces| user_interfaces.remove(&id));
        }

        if let Some(_window) = self.window_manager.remove(id) {
            // TODO: handle clipboard stuff here, if needed

            self.events
                .push((id, core::Event::Window(core::window::Event::Closed)));
        }

        if self.window_manager.is_empty() {
            self.compositor = None;
        }
    }

    fn on_timer_wake(&mut self, now: Instant) -> TimeoutAction {
        for (_, window) in self.window_manager.iter_mut() {
            if let RedrawRequest::At(redraw_at) = window.redraw_at {
                if redraw_at <= now {
                    window.request_redraw(core::window::RedrawRequest::NextFrame);
                }
            }
        }

        if let Some(redraw_at) = self.window_manager.redraw_at() {
            TimeoutAction::ToInstant(redraw_at)
        } else {
            TimeoutAction::Drop
        }
    }

    fn schedule_wake_if_needed(&mut self) {
        let mut loop_timer = self.loop_timer_handle.as_source_mut();

        if let Some(new) = self.window_manager.redraw_at()
            && loop_timer
                .current_deadline()
                .is_none_or(|current| current > new)
        {
            loop_timer.set_deadline(new);
            self.loop_handle.update(&self.loop_timer_token).unwrap();
        }
    }

    fn about_to_wait(&mut self) {
        if self.actions > 0 {
            // TODO: free aciton backpressure here
            self.actions = 0;
        }

        if self.events.is_empty() && self.messages.is_empty() && self.window_manager.is_idle() {
            return;
        }

        let mut uis_stale = false;

        let program_wrapper = self.program_wrapper.as_mut().unwrap();

        for (id, window) in self.window_manager.iter_mut() {
            let interact_span = debug::interact(id);

            let mut window_events = vec![];

            // NOTE: this is possible to do without cloning the event with Vec::extract_if in rust 1.87
            self.events.retain(|(window_id, event)| {
                if *window_id == id {
                    window_events.push(event.clone());
                    false
                } else {
                    true
                }
            });

            if window_events.is_empty() && self.messages.is_empty() {
                continue;
            }

            let (ui_state, statuses) =
                program_wrapper.with_user_interfaces_mut(|user_interfaces| {
                    user_interfaces
                        .get_mut(&id)
                        .expect("Get user interface")
                        .update(
                            &window_events,
                            window.state.cursor(),
                            &mut window.renderer,
                            &mut self.clipboard,
                            &mut self.messages,
                        )
                });

            match ui_state {
                user_interface::State::Updated {
                    redraw_request,
                    mouse_interaction,
                    ..
                } => {
                    for pointer in window.pointers.iter() {
                        if let Some(data) = pointer.data::<PointerData>()
                            && let Some(themed_pointer) = self.pointers.get(data.seat())
                        {
                            let _ = themed_pointer
                                .set_cursor(&self.conn, conversion::mouse::icon(mouse_interaction));
                        }
                    }
                    window.update_mouse(mouse_interaction);
                    window.request_redraw(redraw_request);
                }
                user_interface::State::Outdated => {
                    uis_stale = true;
                }
            }

            for (event, status) in window_events.into_iter().zip(statuses.into_iter()) {
                self.runtime.broadcast(subscription::Event::Interaction {
                    window: id,
                    event,
                    status,
                });
            }

            interact_span.finish();
        }

        for (id, event) in self.events.drain(..) {
            self.runtime.broadcast(subscription::Event::Interaction {
                window: id,
                event,
                status: core::event::Status::Ignored,
            });
        }

        if !self.messages.is_empty() || uis_stale {
            let mut program_wrapper = self.program_wrapper.take().unwrap();

            let mut cached_user_interfaces: FxHashMap<core::window::Id, user_interface::Cache> =
                program_wrapper.with_user_interfaces_mut(|user_interfaces| {
                    user_interfaces
                        .drain()
                        .map(|(id, ui)| (id, ui.into_cache()))
                        .collect()
                });

            let mut program = program_wrapper.into_heads().program;

            for message in self.messages.drain(..) {
                let task = self.runtime.enter(|| program.update(message));

                if let Some(stream) = runtime::task::into_stream(task) {
                    self.runtime.run(stream);
                }
            }

            let subscription = self.runtime.enter(|| program.subscription());
            let recipes = subscription::into_recipes(subscription.map(Action::Output));

            self.runtime.track(recipes);

            for (id, window) in self.window_manager.iter_mut() {
                window.state.synchronize(&program, id);
                window.request_redraw(core::window::RedrawRequest::NextFrame);
            }

            debug::theme_changed(|| {
                self.window_manager
                    .first()
                    .and_then(|window| theme::Base::palette(window.state.theme()))
            });

            self.program_wrapper = Some(
                ProgramWrapperBuilder {
                    program,
                    user_interfaces_builder: |program| {
                        cached_user_interfaces
                            .drain()
                            .filter_map(|(id, cache)| {
                                self.window_manager.get_mut(id).map(|window| {
                                    (
                                        id,
                                        build_user_interface(
                                            program,
                                            cache,
                                            &mut window.renderer,
                                            window.state.logical_size(),
                                            id,
                                        ),
                                    )
                                })
                            })
                            .collect()
                    },
                }
                .build(),
            );
        }

        self.schedule_wake_if_needed();
    }

    fn run_action(&mut self, action: Action<P::Message>) {
        // use crate::runtime::clipboard;
        use crate::runtime::{layer_shell, system};
        // use crate::runtime::window;

        self.actions += 1;
        match action {
            Action::Output(message) => {
                self.messages.push(message);
            }
            Action::Clipboard(_action) => todo!(),
            Action::Window(_action) => todo!(),
            Action::LayerShell(action) => match action {
                layer_shell::Action::Open(id, settings, sender) => {
                    self.open_layer(id, settings, sender);
                }
                layer_shell::Action::Close(id) => {
                    self.close_window(id);
                }
            },
            Action::System(action) => match action {
                system::Action::QueryInformation(_channel) => {
                    #[cfg(feature = "system")]
                    {
                        if let Some(compositor) = self.compositor.as_mut() {
                            let graphics_info = compositor.fetch_information();

                            let _ = std::thread::spawn(move || {
                                let information = crate::system::information(graphics_info);

                                let _ = _channel.send(information);
                            });
                        }
                    }
                }
            },
            Action::Widget(operation) => {
                let mut current_operation = Some(operation);

                let program_wrapper = self.program_wrapper.as_mut().unwrap();
                program_wrapper.with_user_interfaces_mut(|user_interfaces| {
                    while let Some(mut operation) = current_operation.take() {
                        for (id, ui) in user_interfaces.iter_mut() {
                            if let Some(window) = self.window_manager.get_mut(*id) {
                                ui.operate(&window.renderer, operation.as_mut());
                            }
                        }

                        match operation.finish() {
                            operation::Outcome::None => {}
                            operation::Outcome::Some(()) => {}
                            operation::Outcome::Chain(next) => {
                                current_operation = Some(next);
                            }
                        }
                    }
                });
            }
            Action::LoadFont { bytes, channel } => {
                if let Some(compositor) = &mut self.compositor {
                    // TODO: Error handling (?)
                    compositor.load_font(bytes.clone());

                    let _ = channel.send(Ok(()));
                }
            }
            Action::Reload => {
                let program_wrapper = self.program_wrapper.as_mut().unwrap();
                program_wrapper.with_mut(|fields| {
                    for (id, window) in self.window_manager.iter_mut() {
                        let Some(ui) = fields.user_interfaces.remove(&id) else {
                            continue;
                        };

                        let cache = ui.into_cache();
                        let size = window.size();

                        let _ = fields.user_interfaces.insert(
                            id,
                            build_user_interface(
                                fields.program,
                                cache,
                                &mut window.renderer,
                                size,
                                id,
                            ),
                        );

                        window.request_redraw(RedrawRequest::NextFrame);
                    }
                });
            }
            Action::Exit => self.exit(None),
        }
    }

    fn exit(&mut self, error: Option<Error>) {
        self.error = error;
        self.loop_signal.stop();
        self.loop_signal.wakeup();
    }
}

/// Builds a window's [`UserInterface`] for the [`Program`].
fn build_user_interface<'a, P: Program + 'static>(
    program: &'a program::Instance<P>,
    cache: user_interface::Cache,
    renderer: &mut P::Renderer,
    size: core::Size,
    id: core::window::Id,
) -> UserInterface<'a, P::Message, P::Theme, P::Renderer>
where
    P::Theme: theme::Base,
{
    let view_span = debug::view(id);
    let view = program.view(id);
    view_span.finish();

    let layout_span = debug::layout(id);
    let user_interface = UserInterface::build(view, size, cache, renderer);
    layout_span.finish();

    user_interface
}

sctk::delegate_compositor!(@<P: Program + 'static> State<P>);
sctk::delegate_keyboard!(@<P: Program + 'static> State<P>);
sctk::delegate_layer!(@<P: Program + 'static> State<P>);
sctk::delegate_output!(@<P: Program + 'static> State<P>);
sctk::delegate_pointer!(@<P: Program + 'static> State<P>);
sctk::delegate_registry!(@<P: Program + 'static> State<P>);
sctk::delegate_seat!(@<P: Program + 'static> State<P>);
sctk::delegate_shm!(@<P: Program + 'static> State<P>);
sctk::delegate_touch!(@<P: Program + 'static> State<P>);

delegate_noop!(@<P: Program + 'static> State<P>: ZwpTextInputManagerV3);

impl<P: Program + 'static> CompositorHandler for State<P> {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: i32,
    ) {
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
        // TODO: does this need to be handled?
    }

    fn frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        _: u32,
    ) {
        let Some(compositor) = &mut self.compositor else {
            return;
        };
        let Some((id, window)) = self.window_manager.get_mut_alias(surface) else {
            return;
        };
        window.redraw_at = core::window::RedrawRequest::Wait;

        let physical_size = window.state.physical_size();

        if physical_size.width == 0 || physical_size.height == 0 {
            return;
        }

        let program_wrapper = self.program_wrapper.as_mut().unwrap();

        if window.viewport_version != window.state.viewport_version() {
            let logical_size = window.state.logical_size();

            program_wrapper.with_user_interfaces_mut(|user_interfaces| {
                let layout_span = debug::layout(id);
                let ui = user_interfaces.remove(&id).expect("Remove user interface");

                let _ = user_interfaces.insert(id, ui.relayout(logical_size, &mut window.renderer));
                layout_span.finish();
            });

            compositor.configure_surface(
                &mut window.surface,
                physical_size.width,
                physical_size.height,
            );

            window.viewport_version = window.state.viewport_version();
        }

        let redraw_event =
            core::Event::Window(core::window::Event::RedrawRequested(Instant::now()));

        let cursor = window.state.cursor();

        let ui_state = program_wrapper.with_user_interfaces_mut(|user_interfaces| {
            let ui = user_interfaces.get_mut(&id).unwrap();

            let draw_span = debug::draw(id);
            let (ui_state, _) = ui.update(
                std::slice::from_ref(&redraw_event),
                cursor,
                &mut window.renderer,
                &mut self.clipboard,
                &mut self.messages,
            );

            ui.draw(
                &mut window.renderer,
                window.state.theme(),
                &renderer::Style {
                    text_color: window.state.text_color(),
                },
                cursor,
            );

            draw_span.finish();

            ui_state
        });

        self.runtime.broadcast(subscription::Event::Interaction {
            window: id,
            event: redraw_event,
            status: core::event::Status::Ignored,
        });

        if let user_interface::State::Updated {
            mouse_interaction,
            redraw_request,
            input_method,
        } = ui_state
        {
            window.request_redraw(redraw_request);
            window.request_input_method(input_method);

            for pointer in window.pointers.iter() {
                if let Some(data) = pointer.data::<PointerData>()
                    && let Some(themed_pointer) = self.pointers.get(data.seat())
                {
                    let _ = themed_pointer
                        .set_cursor(&self.conn, conversion::mouse::icon(mouse_interaction));
                }
            }
            window.update_mouse(mouse_interaction);
        }

        window.draw_preedit();

        let present_span = debug::present(id);
        let present_ok = compositor.present(
            &mut window.renderer,
            &mut window.surface,
            window.state.viewport(),
            window.state.background_color(),
            || {},
        );
        present_span.finish();

        match present_ok {
            Err(error @ compositor::SurfaceError::OutOfMemory) => {
                // This is an unrecoverable error.
                panic!("{error:?}");
            }

            Err(error) => {
                log::error!("Error {error:?} when presenting surface.");

                // Try rendering all windows again next frame.
                for (_, window) in self.window_manager.iter_mut() {
                    window.request_redraw(core::window::RedrawRequest::NextFrame);
                }
            }
            _ => {}
        }
    }

    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl<P: Program + 'static> OutputHandler for State<P> {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, output: wl_output::WlOutput) {
        // TODO: add more info
        self.runtime
            .broadcast(subscription::Event::PlatformSpecific(
                subscription::PlatformSpecific::Wayland(subscription::Wayland::OutputAdded(
                    self.output_state
                        .info(&output)
                        .and_then(|o| o.name)
                        .unwrap_or_default(),
                )),
            ));
    }

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        // TODO: handle this
    }

    fn output_destroyed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        self.runtime
            .broadcast(subscription::Event::PlatformSpecific(
                subscription::PlatformSpecific::Wayland(subscription::Wayland::OutputRemoved(
                    self.output_state
                        .info(&output)
                        .and_then(|o| o.name)
                        .unwrap_or_default(),
                )),
            ));
    }
}

impl<P: Program + 'static> LayerShellHandler for State<P> {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer_surface: &LayerSurface) {
        if let Some((id, _)) = self
            .window_manager
            .get_mut_alias(layer_surface.wl_surface())
        {
            self.close_window(id);
        }
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer_surface: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        let surface_size = core::Size::new(configure.new_size.0, configure.new_size.1);

        let Some(InProgressWindow {
            id,
            raw_window,
            sender,
        }) = self.in_progress_windows.remove(layer_surface.wl_surface())
        else {
            if let Some((id, window)) = self
                .window_manager
                .get_mut_alias(layer_surface.wl_surface())
            {
                window.state.resize(surface_size);
                window.request_redraw(core::window::RedrawRequest::NextFrame);
                self.events.push((
                    id,
                    core::Event::Window(core::window::Event::Resized(
                        window.state.viewport().logical_size(),
                    )),
                ));
            }

            return;
        };

        if self.compositor.is_none() {
            let graphics_settings = self.settings.clone().into();
            let default_fonts = self.settings.fonts.clone();
            let window = raw_window.clone();

            let compositor = self.runtime.block_on(async move {
                let mut compositor = <P::Renderer as compositor::Default>::Compositor::new(
                    graphics_settings,
                    window,
                )
                .await;
                if let Ok(compositor) = &mut compositor {
                    for font in default_fonts {
                        compositor.load_font(font.clone());
                    }
                }
                compositor
            });
            match compositor {
                Ok(compositor) => self.compositor = Some(compositor),
                Err(error) => {
                    self.exit(Some(error.into()));
                    return;
                }
            }
        }
        let compositor = self.compositor.as_mut().unwrap();

        debug::theme_changed(|| {
            if self.window_manager.is_empty() {
                let program = self.program_wrapper.as_ref().unwrap().borrow_program();
                theme::Base::palette(&program.theme(id))
            } else {
                None
            }
        });

        let program_wrapper = self.program_wrapper.as_mut().unwrap();
        let window = program_wrapper.with_mut(|fields| {
            let window = self.window_manager.insert(
                id,
                self.qh.clone(),
                raw_window,
                surface_size,
                fields.program,
                compositor,
            );

            let logical_size = window.state.logical_size();
            let _ = fields.user_interfaces.insert(
                id,
                build_user_interface(
                    fields.program,
                    user_interface::Cache::default(),
                    &mut window.renderer,
                    logical_size,
                    id,
                ),
            );

            window
        });

        self.events.push((
            id,
            core::Event::Layer(layer_shell::Event::Opened {
                size: window.size(),
            }),
        ));

        // TODO: clipboard

        let _ = sender.send(id);
        window.request_redraw(RedrawRequest::NextFrame);
    }
}

impl<P: Program + 'static> SeatHandler for State<P> {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: sctk::seat::Capability,
    ) {
        match capability {
            sctk::seat::Capability::Keyboard => {
                if let Ok(keyboard) = self.seat_state.get_keyboard(&self.qh, &seat, None) {
                    let _ = self.keyboards.insert(seat, keyboard);
                }
            }
            sctk::seat::Capability::Pointer => {
                if let Ok(pointer) = self.seat_state.get_pointer_with_theme(
                    &self.qh,
                    &seat,
                    self.shm.wl_shm(),
                    self.wl_compositor.create_surface(&self.qh),
                    sctk::seat::pointer::ThemeSpec::System,
                ) {
                    let _ = self.pointers.insert(seat, pointer);
                }
            }
            sctk::seat::Capability::Touch => {
                if let Ok(touch) = self.seat_state.get_touch(&self.qh, &seat) {
                    let _ = self.touch.insert(seat, touch);
                }
            }
            _ => {}
        }
    }

    fn remove_capability(
        &mut self,
        conn: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: sctk::seat::Capability,
    ) {
        match capability {
            sctk::seat::Capability::Keyboard => {
                if let Some(keyboard) = self.keyboards.remove(&seat)
                    && let Some(id) = self.keyboard_focuses.remove(&keyboard)
                    && let Some(window) = self.window_manager.get_mut(id)
                {
                    window
                        .state
                        .update_modifiers(sctk::seat::keyboard::Modifiers::default());
                    self.events.push((
                        id,
                        core::Event::Keyboard(core::keyboard::Event::ModifiersChanged(
                            core::keyboard::Modifiers::default(),
                        )),
                    ));
                    self.events
                        .push((id, core::Event::Window(core::window::Event::Unfocused)));
                }
            }
            sctk::seat::Capability::Pointer => {
                if let Some(pointer) = self.pointers.remove(&seat) {
                    for (_, window) in self.window_manager.iter_mut() {
                        let _ = window.pointers.remove(pointer.pointer());
                    }
                }
            }
            sctk::seat::Capability::Touch => {
                if let Some(touch) = self.touch.remove(&seat) {
                    self.cancel(conn, qh, &touch);
                }
            }
            _ => {}
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl<P: Program + 'static> KeyboardHandler for State<P> {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        keyboard: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[sctk::seat::keyboard::Keysym],
    ) {
        if let Some((id, _)) = self.window_manager.get_mut_alias(surface) {
            let _ = self.keyboard_focuses.insert(keyboard.clone(), id);
            self.events
                .push((id, core::Event::Window(core::window::Event::Focused)));
        }
    }

    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        keyboard: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _: u32,
    ) {
        let _ = self.keyboard_focuses.remove(keyboard);
        if let Some((id, window)) = self.window_manager.get_mut_alias(surface) {
            window
                .state
                .update_modifiers(sctk::seat::keyboard::Modifiers::default());
            self.events.push((
                id,
                core::Event::Keyboard(core::keyboard::Event::ModifiersChanged(
                    core::keyboard::Modifiers::default(),
                )),
            ));
            self.events
                .push((id, core::Event::Window(core::window::Event::Unfocused)));
        }
    }

    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        keyboard: &wl_keyboard::WlKeyboard,
        _: u32,
        key_event: sctk::seat::keyboard::KeyEvent,
    ) {
        if let Some(&id) = self.keyboard_focuses.get(keyboard)
            && let Some(window) = self.window_manager.get_mut(id)
        {
            let key = conversion::keyboard::key(key_event.keysym);
            self.events.push((
                id,
                core::Event::Keyboard(core::keyboard::Event::KeyPressed {
                    key: key.clone(),
                    modified_key: key.clone(), // TODO: actually get modified key
                    physical_key: conversion::keyboard::code(key_event.keysym, key_event.raw_code),
                    location: conversion::keyboard::location(key_event.keysym),
                    modifiers: conversion::keyboard::modifiers(window.state.modifiers()),
                    text: key_event.utf8.map(SmolStr::new),
                }),
            ));
        }
    }

    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        keyboard: &wl_keyboard::WlKeyboard,
        _: u32,
        key_event: sctk::seat::keyboard::KeyEvent,
    ) {
        if let Some(&id) = self.keyboard_focuses.get(keyboard)
            && let Some(window) = self.window_manager.get_mut(id)
        {
            let key = conversion::keyboard::key(key_event.keysym);
            self.events.push((
                id,
                core::Event::Keyboard(core::keyboard::Event::KeyReleased {
                    key: key.clone(),
                    modified_key: key.clone(), // TODO: actually get modified key
                    physical_key: conversion::keyboard::code(key_event.keysym, key_event.raw_code),
                    location: conversion::keyboard::location(key_event.keysym),
                    modifiers: conversion::keyboard::modifiers(window.state.modifiers()),
                }),
            ));
        }
    }

    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        keyboard: &wl_keyboard::WlKeyboard,
        _: u32,
        modifiers: sctk::seat::keyboard::Modifiers,
        _: u32,
    ) {
        if let Some(&id) = self.keyboard_focuses.get(keyboard)
            && let Some(window) = self.window_manager.get_mut(id)
        {
            window.state.update_modifiers(modifiers);
            self.events.push((
                id,
                core::Event::Keyboard(core::keyboard::Event::ModifiersChanged(
                    conversion::keyboard::modifiers(modifiers),
                )),
            ));
        }
    }
}

impl<P: Program + 'static> PointerHandler for State<P> {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        pointer: &wl_pointer::WlPointer,
        events: &[sctk::seat::pointer::PointerEvent],
    ) {
        use sctk::seat::pointer::PointerEventKind as PEK;

        for sctk::seat::pointer::PointerEvent {
            surface,
            position,
            kind,
        } in events
        {
            if let Some((id, window)) = self.window_manager.get_mut_alias(surface) {
                let position = core::Point::new(position.0 as f32, position.1 as f32);
                match kind {
                    PEK::Enter { .. } => {
                        let _ = window.pointers.insert(pointer.clone());
                        window.state.update_cursor(Some(position));
                        self.events
                            .push((id, core::Event::Mouse(core::mouse::Event::CursorEntered)));
                    }
                    PEK::Motion { .. } => {
                        let scale_factor = window.state.scale_factor();
                        window.state.update_cursor(Some(position));
                        self.events.push((
                            id,
                            core::Event::Mouse(core::mouse::Event::CursorMoved {
                                position: core::Point::new(
                                    position.x / (scale_factor as f32),
                                    position.y / (scale_factor as f32),
                                ),
                            }),
                        ));
                    }
                    PEK::Press { button, .. } => self.events.push((
                        id,
                        core::Event::Mouse(core::mouse::Event::ButtonPressed(
                            conversion::mouse::button(*button),
                        )),
                    )),
                    PEK::Release { button, .. } => self.events.push((
                        id,
                        core::Event::Mouse(core::mouse::Event::ButtonReleased(
                            conversion::mouse::button(*button),
                        )),
                    )),
                    PEK::Axis {
                        horizontal,
                        vertical,
                        ..
                    } => self.events.push((
                        id,
                        core::Event::Mouse(core::mouse::Event::WheelScrolled {
                            delta: core::mouse::ScrollDelta::Pixels {
                                x: horizontal.absolute as f32,
                                y: vertical.absolute as f32,
                            },
                        }),
                    )),
                    PEK::Leave { .. } => {
                        let _ = window.pointers.remove(pointer);
                        window.state.update_cursor(None);
                        self.events
                            .push((id, core::Event::Mouse(core::mouse::Event::CursorLeft)));
                    }
                }
            }
        }
    }
}

impl<P: Program + 'static> TouchHandler for State<P> {
    fn down(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        touch: &wl_touch::WlTouch,
        _: u32,
        _: u32,
        surface: wl_surface::WlSurface,
        touch_id: i32,
        position: (f64, f64),
    ) {
        if let Some((id, window)) = self.window_manager.get_mut_alias(&surface) {
            let position = core::Point::new(position.0 as f32, position.1 as f32);
            let touch_ids = self
                .touches
                .entry(touch.clone())
                .or_insert_with(|| FxHashMap::default());
            let _ = touch_ids.insert(touch_id, (id, position));

            window.state.update_cursor(Some(position));
            self.events.push((
                id,
                core::Event::Touch(core::touch::Event::FingerPressed {
                    id: core::touch::Finger(touch_id as u64),
                    position,
                }),
            ));
        }
    }

    fn up(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        touch: &wl_touch::WlTouch,
        _: u32,
        _: u32,
        touch_id: i32,
    ) {
        if let Some(touch_ids) = self.touches.get_mut(touch)
            && let Some((id, position)) = touch_ids.remove(&touch_id)
        {
            self.events.push((
                id,
                core::Event::Touch(core::touch::Event::FingerLifted {
                    id: core::touch::Finger(touch_id as u64),
                    position,
                }),
            ));
        }
    }

    fn motion(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        touch: &wl_touch::WlTouch,
        _: u32,
        touch_id: i32,
        new_position: (f64, f64),
    ) {
        if let Some(touch_ids) = self.touches.get_mut(touch)
            && let Some((id, position)) = touch_ids.get_mut(&touch_id)
            && let Some(window) = self.window_manager.get_mut(*id)
        {
            *position = core::Point::new(new_position.0 as f32, new_position.1 as f32);
            window.state.update_cursor(Some(*position));
            self.events.push((
                *id,
                core::Event::Touch(core::touch::Event::FingerMoved {
                    id: core::touch::Finger(touch_id as u64),
                    position: *position,
                }),
            ));
        }
    }

    fn shape(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_touch::WlTouch,
        _: i32,
        _: f64,
        _: f64,
    ) {
    }

    fn orientation(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_touch::WlTouch,
        _: i32,
        _: f64,
    ) {
    }

    fn cancel(&mut self, _: &Connection, _: &QueueHandle<Self>, touch: &wl_touch::WlTouch) {
        if let Some(touch_ids) = self.touches.remove(touch) {
            for (touch_id, (id, position)) in touch_ids {
                self.events.push((
                    id,
                    core::Event::Touch(core::touch::Event::FingerLost {
                        id: core::touch::Finger(touch_id as u64),
                        position,
                    }),
                ));
            }
        }
    }
}

impl<P: Program + 'static> ShmHandler for State<P> {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl<P: Program + 'static> ProvidesRegistryState for State<P> {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    registry_handlers![OutputState, SeatState];
}
