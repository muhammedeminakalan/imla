//! The dictation HUD: a small click-through pill at the top of the screen (wlr layer-shell,
//! which KWin supports). Listening shows a red dot and a live level meter; transcribing shows
//! three pulsing dots; an error flashes the pill red. It runs on its own thread with its own
//! Wayland connection and never blocks the dictation loop.

use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

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
use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Transform};
use wayland_client::{
    Connection, QueueHandle,
    globals::registry_queue_init,
    protocol::{wl_output, wl_shm, wl_surface},
};

#[derive(Debug, Clone, Copy)]
pub enum HudMsg {
    Listen,
    Level(f32),
    Busy,
    Error,
    Hide,
}

#[derive(PartialEq, Clone, Copy)]
enum Mode {
    Hidden,
    Listening,
    Busy,
    Error,
}

const W: u32 = 176;
const H: u32 = 44;
const BARS: usize = 14;

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
    mode: Mode,
    levels: VecDeque<f32>,
    since: Instant,
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
        mode: Mode::Hidden,
        levels: VecDeque::from(vec![0.0; BARS]),
        since: Instant::now(),
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

    loop {
        let tick = if hud.mode == Mode::Hidden { None } else { Some(Duration::from_millis(33)) };
        event_loop.dispatch(tick, &mut hud)?;
        if hud.mode == Mode::Error && hud.since.elapsed() > Duration::from_millis(900) {
            hud.hide();
        }
        if hud.mode != Mode::Hidden && hud.layer.is_none() {
            hud.create_layer(&qh); // the compositor closed it (e.g. output change) while visible
        }
        if hud.configured {
            hud.draw();
        }
        conn.flush()?;
    }
}

impl Hud {
    fn handle(&mut self, msg: HudMsg, qh: &QueueHandle<Self>) {
        match msg {
            HudMsg::Listen => {
                self.levels.iter_mut().for_each(|l| *l = 0.0);
                self.set_mode(Mode::Listening, qh);
            }
            HudMsg::Level(l) => {
                if self.mode == Mode::Listening {
                    self.levels.pop_front();
                    self.levels.push_back(l);
                }
            }
            HudMsg::Busy => self.set_mode(Mode::Busy, qh),
            HudMsg::Error => self.set_mode(Mode::Error, qh),
            HudMsg::Hide => self.hide(),
        }
    }

    fn set_mode(&mut self, mode: Mode, qh: &QueueHandle<Self>) {
        self.mode = mode;
        self.since = Instant::now();
        self.create_layer(qh);
    }

    fn create_layer(&mut self, qh: &QueueHandle<Self>) {
        if self.layer.is_none() {
            let surface = self.compositor.create_surface(qh);
            // empty input region: clicks go through to the window below
            if let Ok(region) = Region::new(&self.compositor) {
                surface.set_input_region(Some(region.wl_region()));
            }
            let layer =
                self.layer_shell.create_layer_surface(qh, surface, Layer::Overlay, Some("dictate-hud"), None);
            layer.set_anchor(Anchor::TOP);
            layer.set_margin(18, 0, 0, 0);
            layer.set_keyboard_interactivity(KeyboardInteractivity::None);
            layer.set_size(W, H);
            layer.commit();
            self.layer = Some(layer);
            self.configured = false;
        }
    }

    fn hide(&mut self) {
        self.mode = Mode::Hidden;
        self.layer = None; // dropping the layer surface unmaps it
        self.configured = false;
    }

    fn draw(&mut self) {
        let Some(layer) = &self.layer else { return };
        let Some(mut pm) = Pixmap::new(W, H) else { return };
        let t = self.since.elapsed().as_secs_f32();
        let (w, h) = (W as f32, H as f32);

        let mut paint = Paint { anti_alias: true, ..Paint::default() };
        let bg = if self.mode == Mode::Error {
            Color::from_rgba8(150, 30, 36, 235)
        } else {
            Color::from_rgba8(18, 18, 22, 225)
        };
        paint.set_color(bg);
        if let Some(p) = rounded(0.5, 0.5, w - 1.0, h - 1.0, (h - 1.0) / 2.0) {
            pm.fill_path(&p, &paint, FillRule::Winding, Transform::identity(), None);
        }

        match self.mode {
            Mode::Listening => {
                let pulse = 0.75 + 0.25 * (t * 4.0).sin();
                paint.set_color(Color::from_rgba8(240, 70, 70, (255.0 * pulse) as u8));
                circle(&mut pm, &paint, 22.0, h / 2.0, 6.0);
                let (x0, gap, bw) = (40.0, 9.0, 5.0);
                paint.set_color(Color::from_rgba8(235, 235, 240, 240));
                for (i, l) in self.levels.iter().enumerate() {
                    let bh = (4.0 + l * (h - 16.0)).min(h - 12.0);
                    if let Some(p) = rounded(x0 + i as f32 * gap, (h - bh) / 2.0, bw, bh, bw / 2.0) {
                        pm.fill_path(&p, &paint, FillRule::Winding, Transform::identity(), None);
                    }
                }
            }
            Mode::Busy => {
                for i in 0..3 {
                    let phase = (t * 5.0 - i as f32 * 0.7).sin() * 0.5 + 0.5;
                    paint.set_color(Color::from_rgba8(235, 235, 240, (90.0 + 165.0 * phase) as u8));
                    circle(&mut pm, &paint, w / 2.0 - 20.0 + i as f32 * 20.0, h / 2.0, 4.0 + 1.5 * phase);
                }
            }
            Mode::Error => {
                paint.set_color(Color::from_rgba8(255, 255, 255, 240));
                circle(&mut pm, &paint, w / 2.0, h / 2.0, 5.0);
            }
            Mode::Hidden => return,
        }

        let stride = W as i32 * 4;
        let Ok((buffer, canvas)) = self.pool.create_buffer(W as i32, H as i32, stride, wl_shm::Format::Argb8888)
        else {
            return;
        };
        // tiny-skia gives premultiplied RGBA; wl_shm ARGB8888 is premultiplied BGRA in memory
        for (dst, src) in canvas.chunks_exact_mut(4).zip(pm.data().chunks_exact(4)) {
            dst.copy_from_slice(&[src[2], src[1], src[0], src[3]]);
        }
        let surface = layer.wl_surface();
        surface.damage_buffer(0, 0, W as i32, H as i32);
        if buffer.attach_to(surface).is_ok() {
            layer.commit();
        }
    }
}

fn rounded(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    let r = r.min(w / 2.0).min(h / 2.0);
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.quad_to(x + w, y, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.quad_to(x + w, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.quad_to(x, y + h, x, y + h - r);
    pb.line_to(x, y + r);
    pb.quad_to(x, y, x + r, y);
    pb.close();
    pb.finish()
}

fn circle(pm: &mut Pixmap, paint: &Paint, cx: f32, cy: f32, r: f32) {
    if let Some(p) = PathBuilder::from_circle(cx, cy, r) {
        pm.fill_path(&p, paint, FillRule::Winding, Transform::identity(), None);
    }
}

impl CompositorHandler for Hud {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {}
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
