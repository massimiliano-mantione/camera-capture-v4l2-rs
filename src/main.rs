use anyhow::Result;
use eframe::egui::{self, load::DefaultBytesLoader};
use epaint::textures::TextureOptions;
use epaint::ColorImage;
use image::codecs::jpeg::JpegDecoder;
use image::DynamicImage;
use linux_video::types::{ContentType, FrmIvalDiscrete, In, Mmap};
use linux_video::Stream;
use linux_video::{
    types::{BufferType, CapabilityFlag, FourCc, PixFormat},
    Device,
};
use std::sync::Arc;

const CARD: &'static str = "USB 2.0 Camera: HD USB Camera";
const CAPS_REQ_1: CapabilityFlag = CapabilityFlag::VideoCapture;
const CAPS_REQ_2: CapabilityFlag = CapabilityFlag::ExtPixFormat;
const CAPS_REQ_3: CapabilityFlag = CapabilityFlag::Streaming;

fn get_camera() -> Result<Device> {
    let mut devs = Device::list()?;

    let caps_req = CAPS_REQ_1 | CAPS_REQ_2 | CAPS_REQ_3;
    let dev = loop {
        if let Some(path) = devs.fetch_next()? {
            let dev = Device::open(&path)?;
            let caps = dev.capabilities()?;

            if caps.card() == CARD && caps.device_capabilities() == caps_req {
                break Some(dev);
            } else {
                continue;
            }
        } else {
            break None;
        }
    };

    let device = dev.ok_or_else(|| anyhow::Error::msg("cannot find camera"))?;

    let (pixels, _size, _interval) = device
        .formats(BufferType::VideoCapture)
        .into_iter()
        .find_map(|format| {
            format
                .map(|f| {
                    if f.pixel_format() == FourCc::Mjpeg {
                        Some(f)
                    } else {
                        None
                    }
                })
                .unwrap_or(None)
        })
        .ok_or_else(|| anyhow::Error::msg("MJPG format not supported"))
        .and_then(|format| {
            device
                .sizes(format.pixel_format())
                .into_iter()
                .filter_map(|f| if let Ok(f) = f { Some(f) } else { None })
                .find_map(|size| {
                    if size.sizes().any(|s| s.width() == 320 && s.height() == 240) {
                        Some((format, size))
                    } else {
                        None
                    }
                })
                .ok_or_else(|| anyhow::Error::msg("320x240 resolution not supported"))
        })
        .and_then(|(format, size)| {
            device
                .intervals(format.pixel_format(), 320, 240)
                .into_iter()
                .filter_map(|interval| if let Ok(i) = interval { Some(i) } else { None })
                .find_map(|interval| {
                    interval.try_ref::<FrmIvalDiscrete>().and_then(|frac| {
                        if frac.numerator() == 513 && frac.denominator() == 61612 {
                            Some(interval)
                        } else {
                            None
                        }
                    })
                })
                .map(|interval| (format, size, interval))
                .ok_or_else(|| anyhow::Error::msg("interval 513/61612 not supported"))
        })?;

    let mut capture_format = device.format(BufferType::VideoCapture)?;
    capture_format
        .try_mut::<PixFormat>()
        .map(|pix| {
            pix.set_pixel_format(pixels.pixel_format());
            pix.set_width(320);
            pix.set_height(240);
        })
        .ok_or_else(|| anyhow::Error::msg("cannot set pixel format"))?;
    device.set_format(&mut capture_format)?;

    Ok(device)
}

fn main() -> Result<()> {
    let camera = get_camera()?;
    let stream = camera.stream::<In, Mmap>(ContentType::Video, 4)?;
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([320.0, 240.0]),
        ..Default::default()
    };
    eframe::run_native(
        "My egui App",
        options,
        Box::new(|cc| {
            // This gives us image support:
            egui_extras::install_image_loaders(&cc.egui_ctx);
            cc.egui_ctx
                .add_bytes_loader(Arc::new(DefaultBytesLoader::default()));
            Box::<MyApp>::new(MyApp::new(stream))
        }),
    )
    .map_err(|err| anyhow::Error::msg(err.to_string()))
}

struct MyApp {
    name: String,
    age: u32,
    stream: Stream<In, Mmap>,
}

impl MyApp {
    fn new(stream: Stream<In, Mmap>) -> Self {
        Self {
            name: "Arthur".to_owned(),
            age: 42,
            stream,
        }
    }
}

impl eframe::App for MyApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("My egui Application");
            ui.horizontal(|ui| {
                let name_label = ui.label("Your name: ");
                ui.text_edit_singleline(&mut self.name)
                    .labelled_by(name_label.id);
            });
            ui.add(egui::Slider::new(&mut self.age, 0..=120).text("age"));
            if ui.button("Click each year").clicked() {
                self.age += 1;
            }
            ui.label(format!("Hello '{}', age {}", self.name, self.age));

            self.stream
                .next()
                .map_err(|err| anyhow::Error::msg(format!("cannot get frame: {}", err)))
                .and_then(|buf_ref| {
                    let locked = buf_ref.lock();
                    let buf = locked.as_ref();
                    JpegDecoder::new(buf)
                        .map_err(|err| anyhow::Error::msg(format!("cannot get decoder: {}", err)))
                        .and_then(|decoder| {
                            DynamicImage::from_decoder(decoder)
                                .map_err(|err| {
                                    anyhow::Error::msg(format!("cannot get decoder: {}", err))
                                })
                                .map(|img| img.to_rgba8())
                        })
                        .and_then(|rgba8| {
                            let image = ColorImage::from_rgba_unmultiplied([320, 240], &rgba8);
                            let texture = ctx.load_texture("frame", image, TextureOptions::LINEAR);
                            ui.image((texture.id(), texture.size_vec2()));
                            Ok(())
                        })
                })
                .map_err(|err| {
                    println!("error displaying frame: {}", &err);
                    err
                })
                .ok();
        });

        // tell egui to keep rendering
        ctx.request_repaint();
    }
}
