//! DXGI desktop duplication: the GPU keeps a copy of one monitor's image
//! and tells us when it changes. We copy just our area into a small
//! staging texture and read it back only when something was presented, so
//! an idle screen costs a timed-out wait and nothing else.
//!
//! The duplicated image has no cursor; it's drawn in by [`CursorPainter`].

use std::io;

use windows::{
    core::Interface,
    Win32::{
        Foundation::HMODULE,
        Graphics::{
            Direct3D::D3D_DRIVER_TYPE_UNKNOWN,
            Direct3D11::{
                D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_BOX,
                D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE,
                D3D11_MAP_READ, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
            },
            Dxgi::{
                Common::{
                    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_MODE_ROTATION_IDENTITY, DXGI_SAMPLE_DESC,
                },
                CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput1,
                IDXGIOutputDuplication, IDXGIResource, DXGI_ERROR_ACCESS_LOST,
                DXGI_ERROR_NOT_FOUND, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
            },
        },
    },
};

use super::{
    cursor::{CursorPainter, CursorState},
    Area,
};

/// How long to wait for the first image when starting.
const FIRST_FRAME_MS: u32 = 1000;

pub struct Dxgi {
    area: Area,
    /// The area's top-left within its monitor.
    offset: (u32, u32),
    output: IDXGIOutput1,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    dup: IDXGIOutputDuplication,
    /// Area-sized, CPU-readable copy target.
    staging: ID3D11Texture2D,
    /// The desktop image, without the cursor.
    clean: Vec<u8>,
    /// `clean` with the cursor drawn in (when drawing it).
    out: Vec<u8>,
    painter: Option<CursorPainter>,
    /// The cursor last drawn into `out`, and where.
    drawn: Option<(CursorState, (usize, usize, usize, usize))>,
    /// The first frame hasn't been handed out yet.
    fresh: bool,
}

fn err(e: windows::core::Error) -> io::Error {
    io::Error::other(format!("desktop duplication: {}", e.message()))
}

impl Dxgi {
    /// Fails (so the caller can fall back to GDI) if the area isn't
    /// entirely on one unrotated monitor or duplication isn't available.
    pub fn new(area: Area, draw_cursor: bool) -> io::Result<Self> {
        unsafe {
            let (adapter, output, origin) = find_output(area)?;
            let mut device = None;
            let mut context = None;
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .map_err(err)?;
            let (device, context): (ID3D11Device, ID3D11DeviceContext) = (
                device.ok_or_else(|| io::Error::other("no D3D11 device"))?,
                context.ok_or_else(|| io::Error::other("no D3D11 context"))?,
            );
            let dup = output.DuplicateOutput(&device).map_err(err)?;
            let desc = D3D11_TEXTURE2D_DESC {
                Width: area.width,
                Height: area.height,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_STAGING,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                ..Default::default()
            };
            let mut staging = None;
            device
                .CreateTexture2D(&desc, None, Some(&mut staging))
                .map_err(err)?;
            let len = area.width as usize * area.height as usize * 4;
            let mut d = Self {
                area,
                offset: ((area.x - origin.0) as u32, (area.y - origin.1) as u32),
                output,
                device,
                context,
                dup,
                staging: staging.ok_or_else(|| io::Error::other("no staging texture"))?,
                clean: vec![0; len],
                out: if draw_cursor {
                    vec![0; len]
                } else {
                    Vec::new()
                },
                painter: if draw_cursor {
                    CursorPainter::new()
                } else {
                    None
                },
                drawn: None,
                fresh: true,
            };
            // Prove it works now, while falling back to GDI is still easy.
            if !d.acquire(FIRST_FRAME_MS)? {
                return Err(io::Error::other("desktop duplication gave no first frame"));
            }
            Ok(d)
        }
    }

    /// Wait up to `timeout_ms` for a new desktop image and copy our area
    /// of it into `clean`. Returns whether the image changed.
    fn acquire(&mut self, timeout_ms: u32) -> io::Result<bool> {
        unsafe {
            let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut resource: Option<IDXGIResource> = None;
            match self
                .dup
                .AcquireNextFrame(timeout_ms, &mut info, &mut resource)
            {
                Ok(()) => {}
                Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(false),
                Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => {
                    // Mode change, UAC prompt, full-screen app: start over
                    // and keep showing the last image meanwhile.
                    self.dup = self.output.DuplicateOutput(&self.device).map_err(err)?;
                    return Ok(false);
                }
                Err(e) => return Err(err(e)),
            }
            // Only cursor movement: the image itself didn't change.
            let presented = info.LastPresentTime != 0 || self.fresh;
            let copied = match (presented, resource) {
                (true, Some(r)) => {
                    let texture: ID3D11Texture2D = r.cast().map_err(err)?;
                    let (x, y) = self.offset;
                    let region = D3D11_BOX {
                        left: x,
                        top: y,
                        front: 0,
                        right: x + self.area.width,
                        bottom: y + self.area.height,
                        back: 1,
                    };
                    self.context.CopySubresourceRegion(
                        &self.staging,
                        0,
                        0,
                        0,
                        0,
                        &texture,
                        0,
                        Some(&region),
                    );
                    true
                }
                _ => false,
            };
            // Hand the frame back as soon as the copy is queued.
            self.dup.ReleaseFrame().map_err(err)?;
            if copied {
                self.read_staging()?;
            }
            Ok(copied)
        }
    }

    fn read_staging(&mut self) -> io::Result<()> {
        unsafe {
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.context
                .Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .map_err(err)?;
            let row = self.area.width as usize * 4;
            let pitch = mapped.RowPitch as usize;
            let src = std::slice::from_raw_parts(
                mapped.pData as *const u8,
                pitch * (self.area.height as usize - 1) + row,
            );
            for (y, dst) in self.clean.chunks_exact_mut(row).enumerate() {
                dst.copy_from_slice(&src[y * pitch..y * pitch + row]);
            }
            self.context.Unmap(&self.staging, 0);
        }
        Ok(())
    }

    pub fn grab(&mut self) -> io::Result<Option<&[u8]>> {
        let fresh = std::mem::take(&mut self.fresh);
        let changed = self.acquire(0)? || fresh;
        let Some(painter) = self.painter.as_mut() else {
            return Ok(changed.then_some(&self.clean[..]));
        };
        let cursor = CursorPainter::state().filter(|c| {
            // Only worth drawing when it's near the area.
            let (x, y) = (c.x() - self.area.x, c.y() - self.area.y);
            x > -256 && y > -256 && x < self.area.width as i32 && y < self.area.height as i32
        });
        if !changed && cursor == self.drawn.map(|d| d.0) {
            return Ok(None);
        }
        let row = self.area.width as usize * 4;
        if changed {
            self.out.copy_from_slice(&self.clean);
        } else if let Some((_, (x, y, w, h))) = self.drawn {
            // Only the cursor moved: undo it, cheaply.
            for r in y..y + h {
                let at = r * row + x * 4;
                self.out[at..at + w * 4].copy_from_slice(&self.clean[at..at + w * 4]);
            }
        }
        self.drawn =
            cursor.and_then(|c| painter.paint(&mut self.out, self.area, c).map(|r| (c, r)));
        if cursor.is_some() && self.drawn.is_none() {
            // Off the area: remember it so a still cursor stays a no-op.
            self.drawn = cursor.map(|c| (c, (0, 0, 0, 0)));
        }
        Ok(Some(&self.out))
    }
}

/// The adapter and output whose monitor fully contains `area`, and that
/// monitor's top-left on the desktop.
unsafe fn find_output(area: Area) -> io::Result<(IDXGIAdapter1, IDXGIOutput1, (i32, i32))> {
    let factory: IDXGIFactory1 = CreateDXGIFactory1().map_err(err)?;
    let (right, bottom) = (area.x + area.width as i32, area.y + area.height as i32);
    for a in 0.. {
        let adapter = match factory.EnumAdapters1(a) {
            Ok(adapter) => adapter,
            Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(e) => return Err(err(e)),
        };
        for o in 0.. {
            let output = match adapter.EnumOutputs(o) {
                Ok(output) => output,
                Err(_) => break,
            };
            let desc = output.GetDesc().map_err(err)?;
            let r = desc.DesktopCoordinates;
            let inside =
                area.x >= r.left && area.y >= r.top && right <= r.right && bottom <= r.bottom;
            if inside && desc.Rotation == DXGI_MODE_ROTATION_IDENTITY {
                let output1: IDXGIOutput1 = output.cast().map_err(err)?;
                return Ok((adapter, output1, (r.left, r.top)));
            }
        }
    }
    Err(io::Error::other(
        "area isn't on a single unrotated monitor; using GDI",
    ))
}
