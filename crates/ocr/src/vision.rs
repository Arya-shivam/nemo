use anyhow::{Context, Result, bail};
use fast_image_resize::{FilterType, ResizeAlg, ResizeOptions, Resizer};
use image::RgbImage;
use ort::{session::Session, value::Tensor};
use tokenizers::Tokenizer;

use crate::{load_runtime, models, session};

pub const EMBEDDING_DIM: usize = 512;
pub const MODEL_ID: &str = "tinyclip-vit8m16-int8-r1";
const SIDE: u32 = 224;
const TOKENS: usize = 77;

/// TinyCLIP image and text embeddings, normalized for cosine similarity.
pub struct Vision {
    session: Session,
    tokenizer: Tokenizer,
    image_output: usize,
    text_output: usize,
}

impl Vision {
    pub fn new(threads: usize) -> Result<Self> {
        load_runtime()?;
        let model_path = models::ensure(&models::TINYCLIP)?;
        let tokenizer_path = models::ensure(&models::TINYCLIP_TOKENIZER)?;
        let session = session(&model_path, threads).inspect_err(|_| {
            let _ = std::fs::remove_file(&model_path);
        })?;
        let tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| anyhow::anyhow!(e.to_string()))
            .with_context(|| format!("loading {}", tokenizer_path.display()))?;
        let output = |name: &str| {
            session
                .outputs()
                .iter()
                .position(|out| out.name() == name)
                .with_context(|| format!("TinyCLIP model has no {name} output"))
        };
        Ok(Self {
            image_output: output("image_embeds")?,
            text_output: output("text_embeds")?,
            session,
            tokenizer,
        })
    }

    pub fn image_embedding(&mut self, img: &RgbImage) -> Result<[f32; EMBEDDING_DIM]> {
        let pixels = Tensor::from_array((
            [1usize, 3, SIDE as usize, SIDE as usize],
            image_tensor(img)?,
        ))?;
        let (ids, mask) = self.tokens("")?;
        self.run(pixels, ids, mask, self.image_output)
    }

    pub fn text_embedding(&mut self, text: &str) -> Result<[f32; EMBEDDING_DIM]> {
        let pixels = Tensor::from_array((
            [1usize, 3, SIDE as usize, SIDE as usize],
            vec![0.0; 3 * SIDE as usize * SIDE as usize],
        ))?;
        let (ids, mask) = self.tokens(text)?;
        self.run(pixels, ids, mask, self.text_output)
    }

    fn tokens(&self, text: &str) -> Result<(Tensor<i64>, Tensor<i64>)> {
        let encoded = self
            .tokenizer
            .encode(text, true)
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let eos = self
            .tokenizer
            .token_to_id("<|endoftext|>")
            .context("TinyCLIP tokenizer has no end-of-text token")? as i64;
        let mut ids: Vec<i64> = encoded.get_ids().iter().map(|&id| id as i64).collect();
        if ids.len() > TOKENS {
            ids.truncate(TOKENS);
            ids[TOKENS - 1] = eos;
        }
        let mut mask = vec![1i64; ids.len()];
        ids.resize(TOKENS, eos);
        mask.resize(TOKENS, 0);
        Ok((
            Tensor::from_array(([1usize, TOKENS], ids))?,
            Tensor::from_array(([1usize, TOKENS], mask))?,
        ))
    }

    fn run(
        &mut self,
        pixels: Tensor<f32>,
        ids: Tensor<i64>,
        mask: Tensor<i64>,
        output: usize,
    ) -> Result<[f32; EMBEDDING_DIM]> {
        let outputs = self.session.run(ort::inputs![
            "input_ids" => ids,
            "attention_mask" => mask,
            "pixel_values" => pixels,
        ])?;
        let (_, values) = outputs[output].try_extract_tensor::<f32>()?;
        if values.len() < EMBEDDING_DIM {
            bail!(
                "TinyCLIP returned {} embedding values, expected {EMBEDDING_DIM}",
                values.len()
            );
        }
        let mut embedding = [0.0; EMBEDDING_DIM];
        embedding.copy_from_slice(&values[..EMBEDDING_DIM]);
        normalize(&mut embedding)?;
        Ok(embedding)
    }
}

fn image_tensor(img: &RgbImage) -> Result<Vec<f32>> {
    if img.width() == 0 || img.height() == 0 {
        bail!("cannot embed an empty image");
    }
    // Keep the whole screenshot. Center-cropping a wide screen image discards
    // side panels and can require an enormous intermediate for narrow shots.
    // Padding with CLIP's mean color normalizes to neutral, not fake black content.
    let scale = SIDE as f32 / img.width().max(img.height()) as f32;
    let width = (img.width() as f32 * scale).round().max(1.0) as u32;
    let height = (img.height() as f32 * scale).round().max(1.0) as u32;
    let mut resized = RgbImage::new(width, height);
    Resizer::new().resize(
        img,
        &mut resized,
        &ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Bilinear)),
    )?;
    let left = (SIDE - width) / 2;
    let top = (SIDE - height) / 2;
    let mut tensor = vec![0.0f32; 3 * SIDE as usize * SIDE as usize];
    let plane = (SIDE * SIDE) as usize;
    let mean = [0.48145466, 0.4578275, 0.40821073];
    let std = [0.26862954, 0.2613026, 0.2757771];
    for (x, y, pixel) in resized.enumerate_pixels() {
        let i = ((y + top) * SIDE + x + left) as usize;
        for channel in 0..3 {
            let value = pixel[channel] as f32 / 255.0;
            tensor[channel * plane + i] = (value - mean[channel]) / std[channel];
        }
    }
    Ok(tensor)
}

fn normalize(vector: &mut [f32; EMBEDDING_DIM]) -> Result<()> {
    let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
    if !norm.is_finite() || norm == 0.0 {
        bail!("TinyCLIP returned an invalid embedding");
    }
    for value in vector {
        *value /= norm;
    }
    Ok(())
}
