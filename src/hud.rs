//! The dictation HUD, "Dot scope": a small click-through overlay at the top of the screen (wlr
//! layer-shell, which KWin supports). A field of dots lit by braided traces follows the voice
//! while listening, flows while transcribing, clears when done and turns coral on an error. It
//! runs on its own thread with its own Wayland connection and never blocks the dictation loop;
//! while hidden the thread sleeps.

#[allow(dead_code)]
mod dot_scope;
mod indicator;
mod shader;
#[allow(dead_code)]
mod spring;

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, Region},
    delegate_dispatch2,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{
            EventLoop,
            channel::{self, Channel, Sender},
        },
        calloop_wayland_source::WaylandSource,
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
    },
    shm::{Shm, ShmHandler, slot::SlotPool},
};

use self::indicator::Indicator;
use wayland_client::{
    Connection, QueueHandle,
    globals::registry_queue_init,
    protocol::{wl_output, wl_shm, wl_surface},
};

#[derive(Debug, Clone, Copy)]
pub enum HudMsg {
    Listen,
    /// The microphone's RMS, 0..1 linear.
    Level(f32),
    Busy,
    Error,
    Hide,
}

/// The layer surface in points: room for the widest plate, its bounce and drop shadow.
const W: u32 = 168;
const H: u32 = 60;
const FRAME: Duration = Duration::from_micros(16_667);

/// A synthetic voice (phrases of syllables with breaths) as the linear RMS a microphone
/// would report `time` seconds in, for `hud-demo`.
pub fn demo_rms(time: f32) -> f32 {
    10f32.powf(dot_scope::demo_loudness(time) / 20.0)
}

/// Starts the HUD thread. Without a Wayland session or layer-shell it logs why and returns a
/// sender whose messages are dropped, so dictation still works.
pub fn spawn() -> Sender<HudMsg> {
    let (tx, rx) = channel::channel();
    std::thread::Builder::new()
        .name("hud".into())
        .spawn(move || {
            // a lost compositor connection ends the HUD only; dictation keeps working
            if let Err(e) = run(rx) {
                log::warn!("HUD kapalı: {e:#}");
            }
        })
        .expect("HUD thread");
    tx
}

struct Hud {
    registry_state: RegistryState,
    output_state: OutputState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    shm: Shm,
    pool: SlotPool,
    layer: Option<LayerSurface>,
    configured: bool,
    model: Indicator,
    /// Integer buffer scale of the output the surface is on.
    scale: i32,
    last_step: Instant,
}

fn run(rx: Channel<HudMsg>) -> Result<()> {
    let conn = Connection::connect_to_env().context("Wayland oturumu yok")?;
    let (globals, queue) = registry_queue_init(&conn)?;
    let qh = queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).context("wl_compositor yok")?;
    let layer_shell = LayerShell::bind(&globals, &qh).context("layer-shell yok")?;
    let shm = Shm::bind(&globals, &qh).context("wl_shm yok")?;
    let pool = SlotPool::new((W * H * 4 * 2) as usize, &shm)?;
    let mut hud = Hud {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        compositor,
        layer_shell,
        shm,
        pool,
        layer: None,
        configured: false,
        model: Indicator::new(Instant::now()),
        scale: 1,
        last_step: Instant::now(),
    };

    let mut event_loop: EventLoop<Hud> = EventLoop::try_new()?;
    WaylandSource::new(conn.clone(), queue)
        .insert(event_loop.handle())
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let qh2 = qh.clone();
    event_loop
        .handle()
        .insert_source(rx, move |ev, _, hud| {
            if let channel::Event::Msg(m) = ev {
                hud.handle(m, &qh2);
            }
        })
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let mut next_frame = Instant::now();
    loop {
        // hidden: sleep until a message arrives; visible: wake for the next ~60 fps frame
        let tick = hud.model.active().then(|| next_frame.saturating_duration_since(Instant::now()));
        event_loop.dispatch(tick, &mut hud)?;
        let now = Instant::now();
        if hud.model.active() {
            if hud.layer.is_none() {
                hud.create_layer(&qh); // the compositor closed it (e.g. output change) while visible
            }
            if now >= next_frame {
                hud.frame(now);
                next_frame = if now.duration_since(next_frame) > FRAME * 4 { now + FRAME } else { next_frame + FRAME };
            }
        } else if hud.layer.is_some() {
            hud.destroy_layer(); // faded out: unmap and stop waking up
        }
        conn.flush()?;
    }
}

impl Hud {
    fn handle(&mut self, msg: HudMsg, qh: &QueueHandle<Self>) {
        let now = Instant::now();
        match msg {
            HudMsg::Listen => {
                if !self.model.active() {
                    self.last_step = now;
                }
                self.model.listen(now);
                self.create_layer(qh);
            }
            HudMsg::Level(l) => self.model.level(l),
            HudMsg::Busy => {
                self.model.busy(now);
                self.create_layer(qh);
            }
            HudMsg::Error => {
                self.model.fail(now);
                self.create_layer(qh);
            }
            HudMsg::Hide => self.model.hide(now),
        }
    }

    fn create_layer(&mut self, qh: &QueueHandle<Self>) {
        if self.layer.is_none() {
            self.last_step = Instant::now();
            let surface = self.compositor.create_surface(qh);
            // empty input region: clicks go through to the window below
            if let Ok(region) = Region::new(&self.compositor) {
                surface.set_input_region(Some(region.wl_region()));
            }
            surface.set_buffer_scale(self.scale);
            let layer =
                self.layer_shell.create_layer_surface(qh, surface, Layer::Overlay, Some("dictate-hud"), None);
            layer.set_anchor(Anchor::TOP);
            layer.set_keyboard_interactivity(KeyboardInteractivity::None);
            layer.set_size(W, H);
            layer.commit();
            self.layer = Some(layer);
            self.configured = false;
        }
    }

    fn destroy_layer(&mut self) {
        self.layer = None; // dropping the layer surface unmaps it
        self.configured = false;
    }

    /// Steps the animation to `now` and draws it.
    fn frame(&mut self, now: Instant) {
        let dt = now.duration_since(self.last_step).as_secs_f32();
        self.last_step = now;
        self.model.step(now, dt);
        if self.configured {
            self.draw();
        }
    }

    fn draw(&mut self) {
        let Some(layer) = &self.layer else { return };
        let scale = self.scale.max(1);
        let (w, h) = (W as i32 * scale, H as i32 * scale);
        let Ok((buffer, canvas)) =
            self.pool.create_buffer(w, h, w * 4, wl_shm::Format::Argb8888)
        else {
            return;
        };
        // premultiplied BGRA, which is what wl_shm ARGB8888 is in memory
        shader::render(&self.model, w as u32, h as u32, scale as f32, canvas);
        let surface = layer.wl_surface();
        surface.damage_buffer(0, 0, w, h);
        if buffer.attach_to(surface).is_ok() {
            layer.commit();
        }
    }
}

impl CompositorHandler for Hud {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        new_factor: i32,
    ) {
        self.scale = new_factor.max(1);
        surface.set_buffer_scale(self.scale);
    }
    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for Hud {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl LayerShellHandler for Hud {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.layer = None; // keep the mode: the run loop recreates the surface
        self.configured = false;
    }
    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &LayerSurface,
        _: LayerSurfaceConfigure,
        _: u32,
    ) {
        self.configured = true;
    }
}

impl ShmHandler for Hud {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for Hud {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState];
}

smithay_client_toolkit::delegate_registry!(Hud);
delegate_dispatch2!(Hud);
