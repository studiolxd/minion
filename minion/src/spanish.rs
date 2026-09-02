//! Spanish-language knowledge used to make matching forgiving.
//!
//! The point is that one natural phrase in the command table should cover
//! the ways people actually say it. "Cierra la ventana", "cerrar ventana"
//! and "cierra ventana" are the same instruction, and the table should not
//! have to list all three.

/// Words carrying no instruction: articles, pronouns, fillers.
///
/// Dropped from both sides before comparing, so their presence or absence
/// never changes the outcome.
const FILLER: &[&str] = &[
    "el", "la", "los", "las", "un", "una", "unos", "unas",
    "lo", "le", "les", "me", "te", "se", "nos",
    "esto", "esta", "este", "estos", "estas", "eso", "esa", "ese", "esos", "esas",
    "de", "del", "al", "a", "en", "con", "para", "por",
    "mi", "tu", "su", "mis", "tus", "sus",
    "que", "y", "o", "favor", "porfa", "venga", "anda",
];

/// Verb forms reduced to a single canonical stem.
///
/// Only the imperative and infinitive of verbs the vocabulary uses. A real
/// stemmer would be overkill and would happily merge words that mean
/// different things — "corta" and "corre" share more than they should.
const VERBS: &[(&str, &str)] = &[
    ("abre", "abrir"), ("abreme", "abrir"), ("abrir", "abrir"),
    ("cierra", "cerrar"), ("cierre", "cerrar"), ("cerrar", "cerrar"),
    ("sal", "salir"), ("sale", "salir"), ("salte", "salir"), ("salir", "salir"),
    ("mata", "matar"), ("matar", "matar"),
    ("termina", "terminar"), ("terminar", "terminar"),
    ("guarda", "guardar"), ("guardar", "guardar"),
    ("copia", "copiar"), ("copiar", "copiar"),
    ("pega", "pegar"), ("pegar", "pegar"),
    ("corta", "cortar"), ("cortalo", "cortar"), ("cortar", "cortar"),
    ("borra", "borrar"), ("borralo", "borrar"), ("borrar", "borrar"), ("elimina", "borrar"),
    ("busca", "buscar"), ("buscar", "buscar"),
    ("deshaz", "deshacer"), ("deshacer", "deshacer"),
    ("rehaz", "rehacer"), ("rehacer", "rehacer"),
    ("selecciona", "seleccionar"), ("seleccionar", "seleccionar"),
    ("minimiza", "minimizar"), ("minimizar", "minimizar"),
    ("recarga", "recargar"), ("recargar", "recargar"),
    ("actualiza", "recargar"), ("actualizar", "recargar"),
    ("sube", "subir"), ("subir", "subir"),
    ("baja", "bajar"), ("bajar", "bajar"),
    ("quita", "quitar"), ("quitar", "quitar"),
    ("pon", "poner"), ("ponme", "poner"), ("poner", "poner"),
    ("para", "parar"), ("parar", "parar"), ("pausa", "parar"), ("pausar", "parar"),
    ("bloquea", "bloquear"), ("bloquear", "bloquear"),
    ("esconde", "ocultar"), ("esconder", "ocultar"),
    ("oculta", "ocultar"), ("ocultar", "ocultar"),
    ("muestra", "mostrar"), ("muestrame", "mostrar"), ("mostrar", "mostrar"),
    ("deja", "dejar"), ("dejar", "dejar"),
    ("vuelve", "volver"), ("volver", "volver"),
    ("retrocede", "volver"), ("retroceder", "volver"), ("atras", "atras"),
    ("avanza", "avanzar"), ("avanzar", "avanzar"),
    ("ve", "ir"), ("vete", "ir"), ("ir", "ir"),
    ("trae", "traer"), ("traeme", "traer"), ("traer", "traer"),
    ("dame", "dar"), ("da", "dar"), ("dar", "dar"),
    ("saca", "sacar"), ("sacame", "sacar"), ("sacar", "sacar"),
    ("lanza", "lanzar"), ("lanzar", "lanzar"),
    ("cambia", "cambiar"), ("cambiar", "cambiar"),
    ("enfoca", "enfocar"), ("enfocar", "enfocar"),
    ("recupera", "recuperar"), ("recuperar", "recuperar"),
    ("reabre", "recuperar"), ("reabrir", "recuperar"),
    ("captura", "capturar"), ("capturar", "capturar"),
    ("recorta", "recortar"), ("recortar", "recortar"),
    ("silencia", "silenciar"), ("silenciar", "silenciar"),
    ("devuelve", "devolver"), ("devolver", "devolver"),
    ("reproduce", "reproducir"), ("reproducir", "reproducir"),
    ("descansa", "descansar"), ("descansar", "descansar"),
    ("duerme", "dormir"), ("duermete", "dormir"), ("dormir", "dormir"),
    ("silenciate", "silenciar"), ("callate", "callar"), ("callar", "callar"),
    ("apagate", "apagar"), ("apaga", "apagar"), ("apagar", "apagar"),
    ("escucha", "escuchar"), ("escuchar", "escuchar"),
    ("pasa", "pasar"), ("pasar", "pasar"),
    ("escribe", "escribir"), ("escriba", "escribir"), ("escribir", "escribir"),
    ("anota", "anotar"), ("anotar", "anotar"),
    ("apunta", "apuntar"), ("apuntar", "apuntar"),
    ("dicta", "dictar"), ("dictar", "dictar"),
];

/// True if the word carries no instruction and can be dropped.
pub fn is_filler(word: &str) -> bool {
    FILLER.contains(&word)
}

/// Reduces a verb to its canonical form, or returns the word untouched.
pub fn canonical_verb(word: &str) -> &str {
    VERBS
        .iter()
        .find(|(form, _)| *form == word)
        .map_or(word, |(_, root)| root)
}

/// Whether the word is a verb this vocabulary knows.
///
/// Used to tell "cierra Safari" from "abre Safari": naming an application
/// must not be enough to launch it when the verb asked for something else.
pub fn is_known_verb(word: &str) -> bool {
    VERBS.iter().any(|(form, root)| *form == word || *root == word)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imperative_and_infinitive_agree() {
        assert_eq!(canonical_verb("cierra"), canonical_verb("cerrar"));
        assert_eq!(canonical_verb("guarda"), canonical_verb("guardar"));
        assert_eq!(canonical_verb("ve"), canonical_verb("vete"));
    }

    #[test]
    fn unknown_words_pass_through() {
        assert_eq!(canonical_verb("chrome"), "chrome");
        assert_eq!(canonical_verb("ventana"), "ventana");
    }

    #[test]
    fn recognises_its_own_verbs() {
        assert!(is_known_verb("cierra"));
        assert!(is_known_verb("cerrar"));
        assert!(is_known_verb("sal"));
        assert!(!is_known_verb("safari"));
        assert!(!is_known_verb("ventana"));
    }

    #[test]
    fn distinct_verbs_stay_distinct() {
        assert_ne!(canonical_verb("corta"), canonical_verb("copia"));
        assert_ne!(canonical_verb("sube"), canonical_verb("baja"));
    }
}
