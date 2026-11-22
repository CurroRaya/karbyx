//! Integración automática con zsh: emite las marcas OSC 133 que forman los
//! bloques, sin tocar la configuración del usuario.
//!
//! forge apunta `ZDOTDIR` a una carpeta propia cuyo `.zshenv` restaura el
//! `ZDOTDIR` original, carga los archivos del usuario como siempre y añade
//! dos ganchos: uno al principio de `precmd` (código de salida) y otro al
//! final (inicio de prompt), más `preexec` (comando y salida).

use std::collections::HashMap;
use std::path::PathBuf;

const ZSHENV: &str = r#"# forge: integración con zsh (marcas OSC 133 para los bloques).
# Generado por forge; se reescribe al arrancar.
if [[ -n "$FORGE_USER_ZDOTDIR" ]]; then
  ZDOTDIR="$FORGE_USER_ZDOTDIR"
else
  unset ZDOTDIR
fi
unset FORGE_USER_ZDOTDIR
[[ -f "${ZDOTDIR:-$HOME}/.zshenv" ]] && builtin source "${ZDOTDIR:-$HOME}/.zshenv"

if [[ -o interactive && -z "$__forge_hooks" ]]; then
  typeset -g __forge_hooks=1 __forge_ran=
  # Primero de precmd: código de salida del comando anterior.
  __forge_precmd_first() {
    local ret=$?
    if [[ -n "$__forge_ran" ]]; then
      builtin printf '\e]133;D;%s\a' "$ret"
      __forge_ran=
    fi
  }
  # Último de precmd: empieza el prompt; marca B al final del prompt.
  __forge_precmd_last() {
    precmd_functions=(__forge_precmd_first ${precmd_functions:#__forge_precmd_(first|last)} __forge_precmd_last)
    builtin printf '\e]133;A\a'
    [[ "$PS1" != *'133;B'* ]] && PS1="$PS1"$'%{\e]133;B\a%}'
  }
  __forge_preexec() {
    __forge_ran=1
    builtin printf '\e]633;E;%s\a' "${1//[[:cntrl:]]/ }"
    builtin printf '\e]133;C\a'
  }
  precmd_functions=(__forge_precmd_first $precmd_functions __forge_precmd_last)
  preexec_functions=($preexec_functions __forge_preexec)
fi
"#;

/// Escribe el script (si cambió) y devuelve las variables de entorno que
/// activan la integración, o `None` si la shell no es zsh.
pub fn zsh_env(shell: &str) -> Option<HashMap<String, String>> {
    if !shell.ends_with("zsh") {
        return None;
    }
    let dir = integration_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    let file = dir.join(".zshenv");
    if std::fs::read_to_string(&file).ok().as_deref() != Some(ZSHENV) {
        std::fs::write(&file, ZSHENV).ok()?;
    }
    let mut env = HashMap::new();
    if let Ok(orig) = std::env::var("ZDOTDIR") {
        env.insert("FORGE_USER_ZDOTDIR".into(), orig);
    }
    env.insert("ZDOTDIR".into(), dir.to_string_lossy().into_owned());
    Some(env)
}

fn integration_dir() -> Option<PathBuf> {
    dirs::cache_dir().map(|c| c.join("forge").join("shell").join("zsh"))
}

/// El script, para tests.
pub fn zshenv_script() -> &'static str {
    ZSHENV
}
