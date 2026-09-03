# Auditoría de Minion

Revisión del código, la seguridad, el rendimiento, la interfaz y el repositorio.
Estado a 3 de septiembre de 2026, commit `af1690c`. No se modificó ningún fichero.

| Métrica | Valor |
|---|---|
| Líneas de Rust | 8 043 |
| Tests | 132 / 132 en verde |
| Avisos de clippy | 5 |
| Ficheros con diffs de `cargo fmt` | 19 (137 diffs) |
| CPU en reposo | 0,3 % |
| Hilos | 22 |
| RSS | 987 MB al cargar, 1 157 MB tras uso |
| Binario | 30 MB (sin LTO ni strip) |

## Resumen

Minion funciona y está bien pensado: el reconocimiento va en un hilo aparte, la
verificación de voz corre antes de transcribir, no hay interpolación de shell en
ningún sitio y el log es una herramienta real de ajuste. En reposo consume muy poco.

Los problemas graves están en tres sitios: **lo que puede hacer una voz que se
cuela** (dictado libre, cualquier dominio, salir de apps), **el pipeline de audio**
(remuestreo sin filtro antialias, verificación de voz saltada en frases cortas) y
**la robustez en segundo plano** (errores fatales que nadie ve, bucle de reinicios
con launchd, fichero de ejemplo de configuración roto).

Lo primero que haría, en este orden:

1. Restaurar `config.example.toml` (hoy es una copia del README) y añadir un test que lo parsee.
2. Filtro antialias antes de diezmar a 16 kHz. Es la causa más plausible de «Crash», «Cromwell», «so fuddy».
3. Verificar también las frases cortas y recortar el silencio antes de calcular la huella de voz.
4. Exigir voz registrada y un umbral más alto para dictar texto, abrir dominios arbitrarios y salir de apps.
5. Dejar de escribir en el log el texto dictado.
6. Que los errores fatales se vean: diálogo en español y salida limpia, sin reiniciar cada 10 s.
7. Alias de apps por palabra completa: hoy «gmail» abre Mail y «mallorca punto com» abre Orca.
8. Perfil release con LTO, `strip` y `panic = "abort"`; `cargo fmt`; CI.

## Seguridad y privacidad

La superficie de ataque es la voz: cualquiera al alcance del micro, o un altavoz.
Sin perfil registrado se obedece a todo el mundo; con perfil el umbral 0,32 es
deliberadamente laxo. Lo relevante es qué se alcanza con eso.

### Crítico

**Una voz impostora puede teclear texto arbitrario, abrir cualquier dominio y salir de apps.**
`commands.rs:734, 670, 897` · `main.rs:428`
El dictado teclea en lo que tenga el foco (un Terminal incluido: sin Enter, pero el
siguiente Enter del usuario lo ejecuta). «Ve a X punto com» abre `https://X.tld`
para cualquier X. «Y luego» encadena varias órdenes por frase. También ⌘⌫ en
Finder y ⌃C en Terminal.
*Solución:* escalonar. Exigir perfil registrado y similitud ≥ 0,45 para dictado,
dominios fuera de `SITES`, salir y comandos destructivos. Rechazar dictado si la
app frontal está en `TERMINALS`.

### Alto

**El texto dictado se guarda en claro en el log.** `main.rs:432, 599` · `journal.rs:62`
Contraseñas o mensajes dictados acaban en `minion.log`, en `minion.log.1` y
duplicados en `minion-launch.log` vía stdout.
*Solución:* registrar solo el número de caracteres, con un flag `log_dictation`
para depurar. No imprimir por stdout si no es un TTY.

**Los modelos se descargan sin verificar integridad y desde una rama mutable.**
`models.rs:21-23, 94-110` · `download-model.sh:13, 25, 39`
`resolve/main` cambia con el tiempo; `curl -fL` sigue redirecciones a cualquier
host. Un modelo sustituido cambia en silencio quién es «el dueño». El script
escribe directamente al nombre final: una descarga cortada deja un fichero
truncado que se acepta y falla en cada reinicio.
*Solución:* fijar revisión por SHA de commit, SHA-256 por fichero antes de
renombrar, `--proto '=https' --tlsv1.2 --max-time`.

### Medio

**Un fallo de arranque provoca un bucle de reinicios que abre Ajustes del Sistema cada 10 s.**
`main.rs:936-956, 1058` · `install.sh:37-39`
`KeepAlive.SuccessfulExit=false` reinicia al salir con error. Cada arranque
vuelve a pedir Accesibilidad y abrir el panel; una descarga fallida reintenta
670 MB cada 10 s.
*Solución:* salir con 0 en errores de configuración, marcador «ya avisé» para no
reabrir el panel, subir `ThrottleInterval`.

**Un panic en el hilo de escucha deja el icono vivo pero sordo.**
`enroll.rs:64` · `fbank.rs:212` · `main.rs:1042`
El proceso no muere, así que launchd no lo relanza. Hay al menos dos rutas:
`PROMPTS[collected.len()]` fuera de rango tras terminar el registro, y
`partial_cmp().unwrap()` con NaN.
*Solución:* `panic = "abort"` en release más un panic hook que escriba al log;
guardar `accept()` con `if self.finished`.

**Escrituras TOML sin escapar pueden invalidar toda la configuración.**
`learn.rs:102-106` · `preferences.rs:742, 747` · `config.rs:310`
Una comilla o barra invertida en una frase del recognizer o en el nombre de un
micro rompe el fichero, y entonces `load()` ignora el fichero entero y arranca
con valores por defecto, solo con una línea de log.
*Solución:* serializar con `toml::Value::String` y parsear antes de escribir.

**Minion puede oír y obedecer su propia respuesta hablada.**
`speech.rs:70, 85` · `main.rs:254, 473`
El flag `deaf` se escribe pero nadie lo lee. El vaciado de la cola ocurre 300 ms
tras hablar, pero el segmentador necesita 700 ms de silencio, así que la frase
con la respuesta llega después.
*Solución:* pasar `deaf` a `audio::start` y descartar bloques mientras esté activo.

**El atajo global muere si macOS desactiva el event tap y nunca se reactiva.** `hotkey.rs:51-68`
*Solución:* suscribir `TapDisabledByTimeout`/`ByUserInput` y llamar a `enable()`;
sacar la escritura al log del hilo del tap.

**La rotación del log solo ocurre al arrancar; un demonio de semanas no rota nunca.** `journal.rs:34`
*Solución:* comprobar el tamaño en `write()` cada N líneas y reabrir. Esto también
cubre el fichero huérfano visto hoy: el proceso escribe en un inodo que ya no es
el de `minion.log`.

**`uninstall.sh` dice «Removed» y deja la huella de voz, grabaciones, config y logs.**
*Solución:* listar las rutas y añadir `--purge`.

### Bajo

- **Ficheros sensibles con permisos 0644** (voice.txt, grabaciones, log). `~/Library` es 0700, pero entran en copias de seguridad. Crear con `mode(0o600)`.
- **Búsqueda del modelo en el directorio de trabajo** (`./model`, `../model`) antes que en Application Support, y la variable de entorno sigue llamándose `OYENTE_MODEL`. `main.rs:82-96`
- **`bundle_id` de la config se interpola sin escapar en AppleScript.** `actions.rs:227`
- **«Iniciar al arrancar» escribe `current_exe()` en el plist**: desde un build de desarrollo apunta a `target/`. `startup.rs:37`
- **Las grabaciones se sobrescriben entre días** (`%H-%M-%S` sin fecha) y crecen sin límite. `main.rs:305`
- **`minion enroll` ignora el micrófono configurado.** `enroll.rs:125`

## Rendimiento y pipeline de audio

Medido sobre el proceso en marcha: CPU en reposo 0,2 a 0,3 %, 22 hilos, unas 7
activaciones por segundo, RSS que crece de 987 a 1 157 MB tras unas
transcripciones. El consumo en reposo es bueno. Lo mejorable es la calidad del
audio que llega a los modelos y la memoria.

### Alto

**Diezmado de 48 a 16 kHz sin filtro antialias.** `audio.rs:81-102`
Todo lo que hay entre 8 y 24 kHz se pliega en 0 a 8 kHz. Las fricativas (s, f,
ch) viven entre 4 y 10 kHz: justo «Safari» y «Chrome», los fallos de todas las
rondas. El propio comentario del código señala la función como la que hay que
sustituir. Además, un dispositivo por debajo de 16 kHz pasa a la frecuencia equivocada.
*Solución:* FIR paso bajo de ~31 coeficientes con corte a 7 kHz antes de diezmar,
o `rubato::FastFixedIn`. Coste ~0,1 % de CPU. Probablemente la mejora de
reconocimiento más grande disponible.

**Las frases de menos de 1,2 s no pasan por la verificación de voz.** `main.rs:360-363` · `speaker.rs:29`
`embed()` devuelve `None` y `.flatten()` las deja pasar. «Minion, Chrome» está
justo en el límite: muchas órdenes reales nunca se verifican.
*Solución:* repetir el audio hasta 1,5 s (el truco de wespeaker) en lugar de
saltar la comprobación.

**La huella de voz se calcula sobre ~1 s de silencio por frase.** `audio.rs:236-257` · `speaker.rs:55-63`
Preroll de 320 ms más 700 ms de cola entran en fbank y CMN; la media se desplaza
hacia «habitación» y no hacia «voz». Explica el margen de solo 0,06 entre el peor
propio (0,38) y el umbral (0,32).
*Solución:* recortar la cola de silencio y el preroll subumbral antes de
`embed()`, y medir `MIN_SAMPLES` en muestras de habla. Da margen para subir el
umbral hacia 0,4.

### Medio

- **Sesión ONNX del modelo de voz con valores por defecto**: un hilo por núcleo físico, arena activa. De ahí salen la mayoría de los 22 hilos y parte del suelo de 405 MB. `speaker.rs:43-47`. Misma configuración que Parakeet: 1 hilo intra e inter, `memory_pattern(false)`, `disable_prepacking`. Unos 8 hilos menos, decenas de MB menos.
- **La recarga del modelo tras el descanso añade ~1 s de latencia visible**, en serie, después de que el usuario haya terminado de hablar. `main.rs:377-388`. Que el segmentador avise de «empezó el habla» y recargar en ese momento: la carga se solapa con la frase y la cola de silencio.
- **RSS crece 170 MB y no vuelve**: la arena del encoder crece hasta la frase más larga (12 s de `max_utterance_ms`). `audio.rs:58` · `main.rs:137`. Bajar `max_utterance_ms` a ~8 s o activar `memory.enable_memory_arena_shrinkage`.
- **Asignaciones, un mutex y un memmove O(n) dentro del callback de CoreAudio.** `audio.rs:340-355, 430-435`. Un `Vec` nuevo por callback, `extend_from_slice` sobre un buffer recién vaciado con `mem::take`, y el segmentador hace polling cada 20 ms. Ring buffer SPSC sin bloqueo (`rtrb`) y remuestreo a un buffer preasignado.
- **Perfil release sin optimizar**: solo `opt-level = 3`. Binario de 30 MB con 74 000 símbolos, sin LTO. `Cargo.toml:25`. Añadir `lto = "fat"`, `codegen-units = 1`, `strip = true`, `panic = "abort"`. Binario a 18 a 22 MB.

### Bajo

- **Pesos del banco mel lineales en Hz; Kaldi los calcula lineales en mel.** Desviación pequeña y sistemática respecto al frontend de entrenamiento. Dos líneas con `mel_from_hz`. `fbank.rs:79-84`
- **Timer del hilo principal a 20 Hz aunque no haya ventana**, y el SVG del icono se reparsea y rasteriza en cada parpadeo. `main.rs:68, 821, 840`. Cachear los tres iconos; bajar a 250 ms sin ventana; `setTolerance`.
- **Preroll asigna un `Vec` por bloque silencioso** (50 asignaciones/s en reposo). `audio.rs:249`
- **Dependencias**: `parakeet-rs` arrastra `tokenizers` (10 MB, no usado); 414 crates en el árbol; `ort` es un release candidate. Poco que hacer salvo aguas arriba.

## Interfaz y experiencia de usuario

### Alto

**La descarga inicial de 670 MB no tiene interfaz cuando se lanza como app.** `main.rs:112-116`
El progreso va a `println!`, que en un bundle se pierde. Durante minutos no hay
icono en la barra, y si falla la descarga el proceso sale y launchd lo relanza
cada 10 s.
*Solución:* crear el icono primero, con tooltip «Descargando modelo… 42 %», y un
diálogo con reintento si falla.

**Los errores fatales nunca llegan al usuario.** `main.rs:384, 988, 1058` · `audio.rs:320`
Micro no disponible, modelo corrupto, fallo de recarga: todo va a stderr y el
usuario ve un icono que no hace nada.
*Solución:* un `fatal(mensaje)` que escriba al log y muestre un diálogo en
español antes de salir.

**Si se deniega el micrófono, Minion dice «Listening» y no oye nada.**
No hay comprobación del permiso; cpal entrega silencio.
*Solución:* detectar energía cero durante N segundos tras arrancar, o consultar
el estado de autorización, y ofrecer «Abrir Ajustes».

**Tres sitios dicen «Reinicia Minion desde el menú» y el menú no tiene esa opción.**
`preferences.rs:782` · `main.rs:804` · `enroll.rs:165`
*Solución:* añadir «Reiniciar», o mejor, recargar en caliente la palabra de
activación y el micro (el stream ya se reabre al cambiar el dispositivo por defecto).

### Medio

- **El campo de palabra de activación guarda y muestra el diálogo de reinicio en cada pulsación de tecla.** `preferences.rs:723-737`. Confirmar al perder el foco o con Enter.
- **La captura del atajo no se puede cancelar**: Escape queda registrado como atajo, y una letra sin modificador se acepta como atajo global (cada «a» que escribas pausa Minion). `preferences.rs:819-843` · `actions.rs:129`. Exigir un modificador, Escape cancela, restaurar el título a los 5 s.
- **⌥Space choca con Alfred, Raycast y remapeos de Spotlight.** El tap es `ListenOnly`: se disparan ambos y no hay señal visible. `config.rs:18`. Un valor por defecto más raro (⌃⌥M), opción «Ninguno», y actualizar el tooltip del icono con «Escuchando» / «En pausa».
- **«No entendido» y «bloqueado por macOS» suenan igual y no muestran nada.** `main.rs:583, 611, 617`. Sonido distinto para BLOCKED y un diálogo único la primera vez con botón a Accesibilidad. Con sonidos apagados no hay feedback de fallo: considerar una cara «confusa» o el tooltip con la última transcripción.
- **Los diálogos son AppleScript**: `ask()` bloquea el timer del hilo principal (icono y preferencias se congelan), y las comillas del mensaje se alteran. `actions.rs:316-341` · `main.rs:791`. Usar `NSAlert` en el hilo principal.
- **El registro de voz está por debajo del pliegue** en una ventana de 640 px con scroll, las instrucciones «1/5, di: …» viven en una etiqueta de dos líneas, no hay cancelar ni «Olvidar mi voz». `preferences.rs:561-591`
- **Sin menú principal no funcionan ⌘V, ⌘W ni ⌘, en Preferencias**, y ningún control tiene etiqueta de accesibilidad. Un `NSApp.mainMenu` mínimo con Edición y Ventana; `setAccessibilityLabel` en cada control.
- **La ayuda es un volcado monoespaciado de ~1 000 frases en un `NSTextField`**, sin búsqueda ni selección. `NSTextView`: ⌘F y copiar funcionan solos.
- **Idiomas mezclados en lo que ve el usuario**: la CLI `learn` mezcla español e inglés, la guía de Accesibilidad va en inglés por stdout. Decidir: stdout de la CLI es para el usuario, en español.

### Bajo

- **Abrir Minion.app cuando ya corre no hace nada visible.** Que la segunda instancia pida a la primera abrir Preferencias.
- **`save_recordings` no está en la interfaz**, aunque es la opción con más impacto en privacidad. Casilla con la carpeta indicada.
- **`minion --help` intenta cargar un modelo llamado «--help».** `main.rs:988`
- **«Say: «minion, abre Chrome»» ignora la palabra de activación configurada.** `main.rs:277`
- macOS 13+ llama «Ajustes» a las preferencias; el slider «Liberar memoria tras» no tiene pista y «Nunca» solo se descubre arrastrando.
- `icon.rs:4` describe una sonrisa hacia abajo que el SVG no tiene.

## Calidad del código y lógica de reconocimiento

Estos casos se verificaron con una copia de la crate y un test sonda sobre
`decide()`; no son hipótesis.

### Alto

**Los alias de apps se buscan como subcadena, sin límite de palabra.** `commands.rs:698, 846`

```
minion abre gmail              → abrir Mail   (0.94, "gmail" contiene "mail")
minion ve a mallorca punto com → abrir Orca
minion abre el editorial       → abrir Orca
```

Y como la ruta web está condicionada a `find_app().is_none()`, la app
equivocada además suprime el `Browse` correcto.
*Solución:* comparar por secuencias de palabras de `spoken_words`; comprobar
nombres exactos de `SITES` antes que las apps.

**`split_chain` rompe el dictado.** `main.rs:373` · `commands.rs:570-590`

```
Minion escribe hola y luego adiós     → ["Minion escribe hola", "Minion adiós"]
(en modo dictado) hola y luego adiós  → teclea "hola hola adiós "
```

*Solución:* no dividir mientras `dictating`; dividir solo si la cabeza empieza
por la palabra de activación y no es una frase de dictado.

### Medio

- **`strict` es código muerto en `words_match`** (`let _ = strict;`). El comentario promete menos holgura en comandos de una palabra; en la práctica «minion contar esto» ejecuta «cortar» al 100 %. `text.rs:113-131`
- **«para» es a la vez muletilla y verbo.** `keywords("para la música")` devuelve solo `["musica"]`, así que «minion, música» pausa en vez de abrir Spotify. `spanish.rs:16, 48`. Canonicalizar verbos antes de quitar muletillas.
- **La tolerancia de la palabra de activación admite palabras comunes**: mínimo, minuto, mina, minero, mínima. «Minuto abre Chrome» abre Chrome. `commands.rs:491-509`. Exigir 4 letras iniciales cuando se permiten 2 ediciones; ampliar el test negativo.
- **Identidad de comandos por string**: un alias que apunte a un `[[commands]]` del usuario, a un comando contextual o a un nombre mal escrito («atras» por «atrás») se ignora en silencio. `configure()` no valida nada. `commands.rs:33, 804, 939`
- **Tests acoplados a ficheros de esta máquina**: rutas `/tmp/claude-501/voces/*.wav` y `voice.txt.roto` en `~/Library`; en otra máquina pasan como no-ops. `speaker.rs:246-356`. `#[ignore]` con variable de entorno o fixtures pequeñas en `tests/fixtures`.
- **La edición de config no tiene tests reales** (el test reimplementa el bucle en vez de llamarlo), escribe en la ruta real y `set_option` no respeta tablas aunque el comentario lo diga. `config.rs:213-292, 466-484`. Funciones puras `with_option(contents, key, value)` con tests.
- **`learn.rs` tiene 0 tests y parsea el log por string** («unknown  » y «»), con el formato definido en otro fichero. `learn.rs:126` · `main.rs:581`
- **Funciones dios en `main.rs`**: `listen_and_obey` (320 líneas, máquina de estados con `continue`), `report()` con 9 argumentos, `run_menu_bar` (270 líneas). Dictado, deshacer y repetir no tienen ningún test. Extraer `struct Session` con un `interpret()` puro.
- **Ni formateador ni CI**: `cargo fmt --check` muestra 137 diffs; no hay `.github/`. Un commit de `cargo fmt` y un workflow macOS con fmt, clippy `-D warnings` y test.

### Bajo

- **Duplicación y restos**: clamp del umbral en dos sitios, `find_app(rest)` calculado dos veces, wrappers `edits_between` e `is_wake_word` que solo delegan, códigos de tecla mitad con nombre y mitad en crudo. 5 avisos de clippy (CLAUDE.md dice 6): 4 son asserts constantes en un test no-op.
- **Apps de esta máquina compiladas en el binario** (Orca, ChatGPT → `com.openai.codex`, teams2). Llevar la cola larga a la configuración por defecto.
- **El umbral es casi un interruptor de dos niveles**: cualquier coincidencia completa se fija en 0,8, por encima del 0,7 por defecto, así que el slider solo actúa por encima de 0,8. `text.rs:98-102`
- **`actions::*` devuelven `bool`** y pierden la causa del fallo; las líneas BLOCKED no se pueden diagnosticar.
- `learn::without_wake_word` no reutiliza `strip_wake_word`: «mini on …» genera alias con un «on» suelto. `learn.rs:185`
- `dictation_text` exige ≥ 4 caracteres: «minion escribe sí» no se reconoce. `commands.rs:633`

## Repositorio y documentación

### Alto

**`minion/config.example.toml` es una copia byte a byte del README** desde el
commit `62f82cb`. Quien siga el README y lo copie a `config.toml` obtiene un
fichero que no parsea y una app con todos los valores por defecto, sin error visible.
*Solución:* `git show 6351c1e:oyente/config.example.toml`, actualizar claves, y
un test con `include_str!` que lo parsee.

**No hay remoto de git.** Todo el historial vive solo en este disco.

### Medio

- **Documentación desfasada**: el README raíz apunta a `oyente/` y a la palabra «ordenador»; el README de Minion dice «30 tests» (hay 132), «970 frases», menú «Registro → Aprender», y lista como pendiente `[[commands]]`, que ya existe. `config.rs:94` dice umbral 0,45; la constante es 0,32.
- **`.gitignore` ignora `oyente/model/*` (muerto) y no ignora `minion/Minion.app/`**, que aparece sin versionar. `minion/model/speaker.onnx` (24 MB) sí está versionado.

### Bajo

- **Versión duplicada**: Cargo.toml `0.1.0`, build-app.sh `1.0.0-beta.1`. Leerla de Cargo.toml y usar `env!("CARGO_PKG_VERSION")`.
- **Sin `cargo audit`** ni licencia en el repo.

## Plan de mejoras por valor y esfuerzo

| # | Mejora | Esfuerzo | Qué arregla |
|---|---|---|---|
| 1 | Restaurar `config.example.toml` + test que lo parsea | 30 min | Trampa para cualquier usuario que siga el README |
| 2 | Filtro antialias antes de diezmar | 1 a 2 h | «Chrome», «Safari» y las demás fricativas |
| 3 | Verificar frases cortas y recortar silencio en `embed()` | 2 h | La garantía «solo mi voz» en las órdenes más habituales |
| 4 | Escalonar permisos por comando y no loguear dictado | 3 h | Lo que puede hacer una voz que se cuela; privacidad del log |
| 5 | Alias por palabra completa, sitios antes que apps | 1 h | gmail → Mail, mallorca → Orca |
| 6 | Verbos antes de muletillas; reactivar `strict`; ajustar wake word | 2 h | música → pausar, contar → cortar, minuto → minion |
| 7 | `split_chain` consciente del dictado | 1 h | Texto duplicado al dictar |
| 8 | Errores fatales visibles + salida limpia + `deaf` real + tap reactivable | 3 h | Robustez en segundo plano |
| 9 | Perfil release, `cargo fmt`, clippy a cero, CI, .gitignore, remoto | 2 h | Higiene del proyecto; binario un tercio más pequeño |
| 10 | Sesión ONNX de voz configurada; recarga anticipada; arena acotada | 2 h | Hilos, memoria, latencia tras el descanso |
| 11 | Descarga con progreso en el icono y checksums fijados | 3 h | Primer arranque e integridad del modelo |
| 12 | Preferencias: wake word por foco, captura cancelable, NSAlert, menú mínimo | 4 h | Los tropiezos de la ventana de Preferencias |
| 13 | Config y learn como funciones puras con tests y escapado TOML | 2 h | Config que se autodestruye |
| 14 | Extraer `Session` de `listen_and_obey` y testear dictado/deshacer/repetir | medio día | La parte menos testeada del núcleo |
| 15 | Actualizar READMEs y CLAUDE.md | 1 h | Docs que contradicen al código |

## Lo que está bien

- Ninguna interpolación de shell: todo es `Command::new(ruta).arg()`; los comandos de terminal se teclean y nunca se envía Enter.
- La verificación de voz corre antes de transcribir y antes de recargar el modelo: otra voz cuesta 10 ms, no 150.
- Configuración de ORT para Parakeet razonada y documentada (1 830 → 934 MB); descarga a `.partial` y renombrado; modelos fuera del bundle; firma con certificado real.
- Separación decidir/ejecutar con un enum `Decision`; tests de invariantes sobre el vocabulario y casos sacados del log real con la razón anotada.
- Los comentarios registran decisiones y callejones sin salida; los tests nunca tocan datos del usuario.
- Segmentador y suelo de ruido adaptativo bien diseñados y testeables con bloques sintéticos; preroll para las consonantes perdidas.
- Menú conforme a las reglas; iconos template que macOS tiñe; sonidos opcionales; solo habla para responder preguntas.
- Preferencias con sistema de `spacing`/`Layout`, sliders vivos, pistas en español bajo cada control, config editada preservando comentarios.
- CPU en reposo del 0,3 % para una app que escucha siempre.
