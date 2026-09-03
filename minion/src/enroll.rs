//! Teaching Minion your voice.
//!
//! Run `minion enroll` and say a few sentences. Each is turned into an
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
pub const SENTENCES: usize = 5;

/// Prompts to read. Varied on purpose — different sounds, different
/// lengths — so the average is a voice and not one turn of phrase.
pub const PROMPTS: &[&str] = &[
    "Hola, esta es mi voz para el ordenador.",
    "Quiero que solo me haga caso a mí cuando hablo.",
    "El reconocimiento funciona mejor si hablo con naturalidad.",
    "Abre el navegador y busca lo que te pido.",
    "Con esto ya debería saber quién soy.",
];

/// Training in progress, shared between the window and the listening loop.
///
/// The window cannot record: the microphone belongs to the recognition
/// thread, which is already receiving utterances. So the window asks, and
/// that thread does the work and reports back through here.
#[derive(Default)]
pub struct Session {
    /// Where the models live, so the profile records which one made it.
    pub model_path: String,
    /// Voices collected so far.
    pub collected: Vec<crate::speaker::Embedding>,
    /// What to show the person right now.
    pub message: String,
    /// Set when there is nothing left to do, successfully or not.
    pub finished: bool,
}

impl Session {
    pub fn starting(model_path: String) -> Self {
        Self {
            model_path,
            collected: Vec::new(),
            message: format!("1/{SENTENCES} — di: «{}»", PROMPTS[0]),
            finished: false,
        }
    }

    /// Takes one utterance. Returns true when training is over.
    pub fn accept(&mut self, embedding: Option<crate::speaker::Embedding>) -> bool {
        // The listening loop and the window clear a finished session on
        // their own schedules, so one more utterance can arrive after the
        // last sentence. There is no sixth prompt to ask for: indexing
        // PROMPTS here is what used to abort the process.
        if self.finished {
            return true;
        }
        let Some(embedding) = embedding else {
            self.message = format!(
                "{}/{SENTENCES} — demasiado corto, repite: «{}»",
                self.collected.len() + 1,
                PROMPTS[self.collected.len()]
            );
            return false;
        };
        self.collected.push(embedding);

        if self.collected.len() < SENTENCES {
            // Numbered by what is being asked for, not by what is already
            // in hand: hearing "1 of 5" while reading the second sentence
            // makes it look like one went missing.
            let asking_for = self.collected.len();
            self.message = format!(
                "{}/{SENTENCES} — di: «{}»",
                asking_for + 1,
                PROMPTS[asking_for]
            );
            return false;
        }

        self.finished = true;
        self.message = match finish(&self.model_path, &self.collected) {
            Ok(agreement) => format!(
                "Listo. Tu voz queda registrada (coherencia {:.0}%).\n\
                 A partir de ahora solo te hará caso a ti.",
                agreement * 100.0
            ),
            Err(problem) => problem,
        };
        true
    }
}

/// Averages, checks and stores what was collected.
fn finish(model_path: &str, collected: &[crate::speaker::Embedding]) -> Result<f32, String> {
    let voice = crate::speaker::average(collected).ok_or("No se recogió nada.")?;
    let agreement = collected
        .iter()
        .map(|sample| crate::speaker::similarity(sample, &voice))
        .fold(f32::MAX, f32::min);

    // Samples that disagree with each other come from a noisy room. Saving
    // them would filter out the very person they are meant to admit.
    if agreement < 0.5 {
        return Err(format!(
            "Las muestras no se parecen entre sí ({:.0}%).\n\
             Prueba en un sitio más silencioso, sin música ni gente cerca.",
            agreement * 100.0
        ));
    }
    crate::speaker::save_profile_for(model_path, &voice)
        .map_err(|e| format!("No se pudo guardar: {e}"))?;
    Ok(agreement)
}

pub fn run(model_path: &str) -> Result<()> {
    let mut model = speaker::Speaker::load(model_path)?;

    println!("\nVamos a aprender tu voz. Di estas cinco frases,");
    println!("con tu tono normal y a la distancia a la que sueles hablarle.\n");

    let active = Arc::new(AtomicBool::new(true));
    // Nothing speaks during enrolment, so nothing ever goes deaf.
    let deaf = Arc::new(AtomicBool::new(false));
    let listener = audio::start(audio::Settings::default(), Arc::clone(&active), None, deaf)?;

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
            if let Some(embedding) = model.embed(utterance.speech()) {
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

    speaker::save_profile_for(model_path, &voice)?;
    println!("Listo. Coherencia entre muestras: {:.0}%.", worst * 100.0);
    println!("Reinicia Minion: a partir de ahora solo te hará caso a ti.");
    println!("Para deshacerlo, borra {}.", speaker::profile_path()
        .map_or_else(|| "el perfil".into(), |p| p.display().to_string()));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_voice(seed: f32) -> crate::speaker::Embedding {
        // Close to each other, so the agreement check passes.
        (0..192).map(|i| seed + i as f32 * 0.001).collect()
    }

    #[test]
    fn the_count_names_what_it_is_asking_for() {
        let mut session = Session::starting(String::new());
        assert!(
            session.message.starts_with("1/5"),
            "should open asking for the first: {}",
            session.message
        );

        for expected in 2..=SENTENCES {
            session.accept(Some(fake_voice(1.0)));
            assert!(
                session.message.starts_with(&format!("{expected}/{SENTENCES}")),
                "after {} samples it should ask for {expected}: {}",
                expected - 1,
                session.message
            );
        }
    }

    #[test]
    fn a_short_utterance_asks_for_the_same_sentence_again() {
        let mut session = Session::starting(String::new());
        session.accept(Some(fake_voice(1.0)));
        let asking = session.message.clone();

        session.accept(None); // too short to use
        assert!(
            session.message.starts_with("2/5"),
            "should still be on the second: {}",
            session.message
        );
        assert_ne!(asking, session.message, "and should say it was too short");
    }

    #[test]
    fn one_utterance_too_many_is_not_a_sixth_prompt() {
        // Whatever arrives after the fifth sentence must not be looked up
        // in PROMPTS: there is no entry there.
        let mut session = Session::starting(String::new());
        for _ in 0..SENTENCES {
            session.accept(Some(fake_voice(1.0)));
        }
        let done = session.message.clone();
        assert!(session.accept(None), "a finished session stays finished");
        assert!(session.accept(Some(fake_voice(1.0))));
        assert_eq!(session.message, done, "and keeps what it had to say");
    }

    #[test]
    fn it_finishes_after_the_last_sentence() {
        let mut session = Session::starting(String::new());
        for _ in 0..SENTENCES - 1 {
            assert!(!session.accept(Some(fake_voice(1.0))), "not done yet");
        }
        assert!(session.accept(Some(fake_voice(1.0))), "the fifth ends it");
        assert!(session.finished);
    }
}
