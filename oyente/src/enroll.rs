//! Teaching Oyente your voice.
//!
//! Run `oyente enroll` and say a few sentences. Each is turned into an
//! embedding and the average is stored; from then on, anything that does
//! not sound like you is discarded before it is even transcribed.
//!
//! Several sentences rather than one: a single phrase carries its own
//! intonation as much as the voice, and averaging cancels that out.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};

use crate::{audio, speaker};

/// How many sentences to collect.
const SENTENCES: usize = 5;

/// Prompts to read. Varied on purpose — different sounds, different
/// lengths — so the average is a voice and not one turn of phrase.
const PROMPTS: &[&str] = &[
    "Hola, esta es mi voz para el ordenador.",
    "Quiero que solo me haga caso a mí cuando hablo.",
    "El reconocimiento funciona mejor si hablo con naturalidad.",
    "Abre el navegador y busca lo que te pido.",
    "Con esto ya debería saber quién soy.",
];

pub fn run(model_path: &str) -> Result<()> {
    let mut model = speaker::Speaker::load(model_path)?;

    println!("\nVamos a aprender tu voz. Di estas cinco frases,");
    println!("con tu tono normal y a la distancia a la que sueles hablarle.\n");

    let active = Arc::new(AtomicBool::new(true));
    let listener = audio::start(audio::Settings::default(), Arc::clone(&active))?;

    let mut collected = Vec::new();
    for (number, prompt) in PROMPTS.iter().enumerate().take(SENTENCES) {
        println!("  {}/{SENTENCES}  «{prompt}»", number + 1);

        // Wait for something long enough to judge. Short noises are
        // rejected by embed(), so this simply keeps listening.
        loop {
            let utterance = listener
                .utterances
                .recv_timeout(Duration::from_secs(60))
                .map_err(|_| anyhow!("no se oyó nada; inténtalo otra vez"))?;
            if let Some(embedding) = model.embed(&utterance) {
                collected.push(embedding);
                println!("        ✓ recogida\n");
                break;
            }
            println!("        (demasiado corto, repítela)");
        }
    }

    let voice = speaker::average(&collected).ok_or_else(|| anyhow!("no se recogió nada"))?;

    // A quick honesty check: every sample should look like the average it
    // came from. If they do not, something is wrong with the recording and
    // saving it would filter out the very person it is meant to admit.
    let worst = collected
        .iter()
        .map(|sample| speaker::similarity(sample, &voice))
        .fold(f32::MAX, f32::min);
    if worst < 0.5 {
        return Err(anyhow!(
            "las muestras no se parecen entre sí ({worst:.2}). Prueba en un sitio \
             más silencioso, sin música ni gente hablando cerca."
        ));
    }

    speaker::save_profile(&voice)?;
    println!("Listo. Coherencia entre muestras: {:.0}%.", worst * 100.0);
    println!("Reinicia Oyente: a partir de ahora solo te hará caso a ti.");
    println!("Para deshacerlo, borra {}.", speaker::profile_path()
        .map_or_else(|| "el perfil".into(), |p| p.display().to_string()));
    Ok(())
}
