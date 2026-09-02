# Oyente

Escucha continua en castellano y ejecuta órdenes. En Rust, sin Handy ni Talon.

## Ejecutar

```bash
cd /Users/suvi/Dev/talon/oyente
./target/release/oyente modelo
```

Di: **«ordenador, abre Chrome»**. También valen Safari, Terminal y Orca, con
cualquiera de estos verbos: abre, ábreme, ve a, vete a, cambia a, pon, ponme,
dame, trae, tráeme, muestra, enfoca.

Imprime **todo** lo que oye, marcando qué hizo con ello:

```
  «Ordenador, abre Chrome.»   ->  abrir Chrome
  «pues no sé qué decirte»    (ignorado)
```

Ctrl-C para salir.

## Cómo está montado

```
micrófono (48 kHz)
    ↓  remuestreo tosco, sin filtro antialias
16 kHz mono
    ↓  detección de voz por energía (RMS)
frase completa
    ↓  Parakeet TDT 0.6b v3, ONNX int8, por CPU
texto en castellano
    ↓  ¿empieza por "ordenador"?
orden ejecutada
```

- **Modelo**: `modelo/` — Parakeet TDT v3 int8 (25 idiomas), descargado de
  `istupakov/parakeet-tdt-0.6b-v3-onnx`. Son 652 MB, no están en git.
- **Crates**: `parakeet-rs` (ONNX Runtime), `cpal` (audio), `anyhow`.
- CoreML está marcado como inestable en la crate, así que va por CPU.

## Los números que habrá que afinar

Están todos juntos arriba de `src/main.rs`:

| Constante | Valor | Qué pasa si está mal |
|---|---|---|
| `UMBRAL_VOZ` | 0.015 | Alto: corta frases. Bajo: transcribe el ventilador |
| `SILENCIO_FIN_MS` | 700 | Bajo: parte frases al pensar. Alto: tarda en responder |
| `MIN_VOZ_MS` | 300 | Filtra golpes y ruidos cortos |
| `MAX_FRASE_MS` | 12000 | Corte de seguridad |

## Lo que falta

- **VAD de verdad** (Silero) en vez de energía. La energía no distingue voz de
  un portazo, y con música de fondo se dispara constantemente.
- **Pulsar teclas**: copiar, guardar, cerrar pestaña. Necesita permiso de
  Accesibilidad para este binario.
- **Contexto por aplicación**: que una orden signifique cosas distintas según
  lo que esté delante.
- **Dictado**: ahora solo ejecuta órdenes, no escribe texto.
- **Arranque automático**: un `launchd` plist para que se inicie solo.
- **Firma del binario**: para que los permisos de micrófono salgan a nombre de
  Oyente y no del terminal desde el que se lanza.

## Historia

Este proyecto sustituye a dos anteriores, ambos retirados:

1. **Talon** (`../talon-archivo/`): excelente precisión, pero su motor
   Conformer solo entiende inglés y no existe versión española.
2. **Handy + script Python** (`../voz/`): entendía castellano de sobra, pero
   sin manos libres — había que pulsar `option+space` para cada orden.

El README de cada uno documenta las trampas que costaron tiempo, por si algún
día hay que volver.
