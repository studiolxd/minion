//! Oyente: escucha continua y ejecuta ordenes dichas en castellano.
//!
//! Prototipo minimo. Solo hace una cosa: oir "ordenador, abre Chrome" y
//! abrir Chrome. Todo lo demas se ignora.
//!
//! El flujo es:
//!   microfono -> remuestreo a 16 kHz -> deteccion de voz por energia
//!             -> Parakeet transcribe la frase -> se busca una orden

use std::process::Command;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parakeet_rs::{ParakeetTDT, Transcriber};

/// Frecuencia que espera el modelo.
const OBJETIVO_HZ: u32 = 16_000;
/// Energia (RMS) por encima de la cual consideramos que alguien habla.
const UMBRAL_VOZ: f32 = 0.015;
/// Silencio necesario para dar la frase por terminada.
const SILENCIO_FIN_MS: usize = 700;
/// Por debajo de esto, es un ruido y no una frase.
const MIN_VOZ_MS: usize = 300;
/// Mas alla de esto cortamos, para no acumular sin fin.
const MAX_FRASE_MS: usize = 12_000;

/// Palabra que convierte una frase en orden.
const PALABRAS_CLAVE: &[&str] = &["ordenador", "ordenadora", "computador"];

/// Bloque de analisis: 20 ms.
const MUESTRAS_BLOQUE: usize = (OBJETIVO_HZ as usize) / 50;

// --- Utilidades de texto --------------------------------------------------

/// Minusculas, sin tildes, sin puntuacion.
fn normalizar(texto: &str) -> String {
    let mut salida = String::with_capacity(texto.len());
    for c in texto.to_lowercase().chars() {
        let limpio = match c {
            'á' | 'à' | 'ä' | 'â' => 'a',
            'é' | 'è' | 'ë' | 'ê' => 'e',
            'í' | 'ì' | 'ï' | 'î' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' => 'o',
            'ú' | 'ù' | 'ü' | 'û' => 'u',
            'ñ' => 'n',
            c if c.is_alphanumeric() => c,
            _ => ' ',
        };
        salida.push(limpio);
    }
    salida.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Aplicaciones que sabemos abrir, con sus formas habladas.
fn buscar_app(frase: &str) -> Option<(&'static str, &'static str)> {
    const APPS: &[(&str, &str, &[&str])] = &[
        ("Chrome", "com.google.Chrome", &["chrome", "crome", "cromo", "navegador"]),
        ("Safari", "com.apple.Safari", &["safari"]),
        ("Terminal", "com.apple.Terminal", &["terminal"]),
        ("Orca", "com.stablyai.orca", &["orca", "orka"]),
    ];
    for (nombre, bundle, alias) in APPS {
        if alias.iter().any(|a| frase.contains(a)) {
            return Some((nombre, bundle));
        }
    }
    None
}

/// Interpreta una transcripcion. Devuelve una descripcion de lo ejecutado.
fn interpretar(texto: &str) -> Option<String> {
    let normal = normalizar(texto);
    let primera = normal.split_whitespace().next()?;
    if !PALABRAS_CLAVE.contains(&primera) {
        return None;
    }
    let resto = normal[primera.len()..].trim();

    // De momento solo entendemos "abre <app>" y variantes.
    let verbos = ["abre", "abreme", "ve a", "vete a", "cambia a", "pon",
                  "ponme", "dame", "trae", "traeme", "muestra", "enfoca"];
    let pide_app = verbos.iter().any(|v| resto.starts_with(v)) || buscar_app(resto).is_some();
    if !pide_app {
        return None;
    }

    let (nombre, bundle) = buscar_app(resto)?;
    Command::new("/usr/bin/open").arg("-b").arg(bundle).spawn().ok()?;
    Some(format!("abrir {nombre}"))
}

// --- Audio ----------------------------------------------------------------

/// Convierte a mono y baja a 16 kHz quedandose una muestra de cada N.
///
/// Es un remuestreo tosco (sin filtro antialias), suficiente para voz en un
/// prototipo. Si la calidad estorba, aqui es donde hay que mejorar.
fn a_16k_mono(datos: &[f32], canales: usize, origen_hz: u32) -> Vec<f32> {
    let paso = (origen_hz as f32 / OBJETIVO_HZ as f32).max(1.0);
    let cuadros = datos.len() / canales;
    let mut salida = Vec::with_capacity((cuadros as f32 / paso) as usize + 1);
    let mut pos = 0.0f32;
    while (pos as usize) < cuadros {
        let i = pos as usize * canales;
        let mezcla: f32 = datos[i..i + canales].iter().sum::<f32>() / canales as f32;
        salida.push(mezcla);
        pos += paso;
    }
    salida
}

fn energia(bloque: &[f32]) -> f32 {
    if bloque.is_empty() {
        return 0.0;
    }
    (bloque.iter().map(|m| m * m).sum::<f32>() / bloque.len() as f32).sqrt()
}

// --- Programa -------------------------------------------------------------

fn main() -> Result<()> {
    println!("Oyente — cargando modelo…");
    let ruta_modelo = std::env::args().nth(1).unwrap_or_else(|| "modelo".into());
    let mut modelo = ParakeetTDT::from_pretrained(&ruta_modelo, None)
        .map_err(|e| anyhow!("no se pudo cargar el modelo de {ruta_modelo}: {e}"))?;
    println!("Modelo listo.");

    let host = cpal::default_host();
    let dispositivo = host
        .default_input_device()
        .ok_or_else(|| anyhow!("no hay microfono disponible"))?;
    let config = dispositivo.default_input_config()?;
    let origen_hz = config.sample_rate();
    let canales = config.channels() as usize;
    println!("Microfono: {origen_hz} Hz, {canales} canal(es)");

    // El callback de audio no debe hacer trabajo pesado: solo deja las
    // muestras ya remuestreadas en una cola que consume el hilo principal.
    let cola = Arc::new(Mutex::new(Vec::<f32>::new()));
    let cola_audio = Arc::clone(&cola);
    let (avisos, recibir_avisos) = mpsc::channel::<String>();

    let stream = dispositivo.build_input_stream(
        config.clone().into(),
        move |datos: &[f32], _: &cpal::InputCallbackInfo| {
            let trozo = a_16k_mono(datos, canales, origen_hz);
            if let Ok(mut c) = cola_audio.lock() {
                c.extend_from_slice(&trozo);
            }
        },
        move |e| {
            let _ = avisos.send(format!("error de audio: {e}"));
        },
        None,
    )?;
    stream.play()?;

    println!("\nEscuchando. Di: «ordenador, abre Chrome»   (Ctrl-C para salir)\n");

    let bloques_silencio_fin = SILENCIO_FIN_MS / 20;
    let bloques_min_voz = MIN_VOZ_MS / 20;
    let muestras_max = MAX_FRASE_MS * OBJETIVO_HZ as usize / 1000;

    let mut frase: Vec<f32> = Vec::new();
    let mut bloques_con_voz = 0usize;
    let mut bloques_de_silencio = 0usize;
    let mut hablando = false;

    loop {
        // Sacamos lo acumulado sin quedarnos con el cerrojo cogido.
        let pendiente: Vec<f32> = {
            let mut c = cola.lock().unwrap();
            if c.len() < MUESTRAS_BLOQUE {
                Vec::new()
            } else {
                std::mem::take(&mut *c)
            }
        };

        if pendiente.is_empty() {
            if let Ok(aviso) = recibir_avisos.try_recv() {
                eprintln!("{aviso}");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
            continue;
        }

        for bloque in pendiente.chunks(MUESTRAS_BLOQUE) {
            let hay_voz = energia(bloque) > UMBRAL_VOZ;

            if hay_voz {
                hablando = true;
                bloques_con_voz += 1;
                bloques_de_silencio = 0;
            } else if hablando {
                bloques_de_silencio += 1;
            }

            if hablando {
                frase.extend_from_slice(bloque);
            }

            let fin_por_silencio = hablando && bloques_de_silencio >= bloques_silencio_fin;
            let fin_por_largo = frase.len() >= muestras_max;

            if fin_por_silencio || fin_por_largo {
                let suficiente = bloques_con_voz >= bloques_min_voz;
                let audio = std::mem::take(&mut frase);
                hablando = false;
                bloques_con_voz = 0;
                bloques_de_silencio = 0;

                if !suficiente {
                    continue; // ruido corto, no merece transcribirse
                }

                match modelo.transcribe_samples(audio, OBJETIVO_HZ, 1, None) {
                    Ok(r) => {
                        let texto = r.text.trim();
                        if texto.is_empty() {
                            continue;
                        }
                        match interpretar(texto) {
                            Some(accion) => println!("  «{texto}»  ->  {accion}"),
                            None => println!("  «{texto}»  (ignorado)"),
                        }
                    }
                    Err(e) => eprintln!("  error al transcribir: {e}"),
                }
            }
        }
    }
}
