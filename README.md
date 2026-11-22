# forge

Terminal para macOS escrita en Rust que une lo mejor de iTerm2 y Warp, con el
consumo de tus asistentes de IA (Claude Code, Codex CLI, Gemini CLI) integrado.

## Estado

| Crate | Qué hace |
|---|---|
| `forge-app` | Binario de la terminal: render por GPU (wgpu + glyphon), pestañas en una barra lateral, paneles divididos, selección y búsqueda, panel de consumo de IA, `forge.toml` con temas y atajos |
| `forge-term` | Sesión de terminal independiente de la UI: PTY + emulador VT (`alacritty_terminal`), selección, búsqueda, protocolos de ratón y snapshot del grid |
| `forge-ai` | Lee el historial de Claude Code (`~/.claude`), Codex CLI (`~/.codex`) y Gemini CLI (`~/.gemini/tmp`), lo normaliza, deduplica y calcula ventanas de 5 h, semana, proyecciones y límites oficiales cuando el proveedor los publica (Codex) |
| `forge-usage` | CLI de consumo: `summary`, `daily`, `blocks`, `models`, `projects`, `watch`, `line`, `--json`, `-p claude\|codex\|gemini` |

## Uso

```bash
cargo run --release -p forge-app          # la terminal
cargo run --release -p forge-usage        # resumen de consumo
cargo run --release -p forge-usage -- watch
```

### Atajos

| Atajo | Acción |
|---|---|
| ⌘T / ⌘⇧C | Nueva pestaña de terminal / del asistente seleccionado |
| ⌘D / ⌘⇧D | Dividir el panel a la derecha / abajo |
| ⌘⌥←→↑↓ | Moverse entre paneles |
| ⌘W | Cerrar el panel activo (o la pestaña si es el último) |
| ⌘1…⌘9, ⌃Tab, ⌘⇧↑/↓ | Cambiar de pestaña |
| ⌘F, ⌘G / ⌘⇧G | Buscar en el historial, siguiente / anterior (↵ y ⇧↵ dentro de la barra) |
| ⌘C / ⌘V | Copiar la selección / pegar (con bracketed paste) |
| ⌘B / ⌘⌥B | Mostrar u ocultar la barra de sesiones / el panel de consumo |
| ⌘+ / ⌘- / ⌘0 | Tamaño de fuente |
| ⌘← / ⌘→, ⌥← / ⌥→, ⌘⌫ | Inicio / fin de línea, palabra anterior / siguiente, borrar línea |
| ⌘K | Limpiar pantalla |
| ⌘Q | Salir |

**Ratón:** arrastrar selecciona, doble clic selecciona una palabra, triple
clic la línea y ⌥+arrastrar un bloque rectangular. Los divisores entre
paneles se arrastran. Si la aplicación pide el ratón (vim, htop, tmux) los
clics le llegan a ella; con ⇧ se fuerza la selección normal.

### Configuración (`~/.config/forge/forge.toml`)

Se aplica al guardar, sin reiniciar:

```toml
[font]
family = "Menlo"
size = 14
line_height = 1.3

[window]
padding = 10

[terminal]
scrollback = 10000
option_as_alt = false      # true: ⌥ envía Meta (ESC+tecla) en vez de @, #, ñ...

[theme]
name = "one-dark"          # one-dark | dracula | solarized-dark | gruvbox | light
# background = "#1a1d23"   # cualquier color suelto sobrescribe el tema

[keys]                     # redefine atajos; "none" desactiva
split_right = "cmd+d"
find = "cmd+f"
```

Acciones configurables: `copy`, `paste`, `quit`, `new_tab`, `new_ai`, `close`,
`next_tab`, `prev_tab`, `toggle_sidebar`, `toggle_usage`, `font_bigger`,
`font_smaller`, `font_reset`, `clear`, `split_right`, `split_down`,
`focus_left`, `focus_right`, `focus_up`, `focus_down`, `find`, `find_next`,
`find_prev`.

### Asistentes y límites

Por defecto forge detecta qué asistentes tienes instalados y muestra una
pestaña por cada uno en el panel derecho. Se puede elegir en
`~/.config/forge/usage.toml`:

```toml
providers = ["auto"]            # o ["claude", "codex", "gemini"]

# Límites propios (Anthropic no publica los de sus planes).
# En tokens "frescos": input + output + escritura de caché.
[limits.claude]
block_tokens = 20_000_000       # ventana de 5 h
weekly_tokens = 300_000_000
weekly_reset = "monday"
```

Codex publica sus límites oficiales en su historial; forge los muestra tal
cual. Los cambios en el archivo se aplican sin reiniciar. El antiguo
`claude.toml` se sigue leyendo si no existe `usage.toml`.

## Desarrollo

```bash
cargo test --workspace
cargo clippy --workspace --all-targets
FORGE_SCREENSHOT=/tmp/forge.png cargo run --release -p forge-app   # vuelca un frame a PNG
FORGE_PROVIDER=codex cargo run --release -p forge-app               # arranca con ese asistente
FORGE_SELFTEST=1 cargo run --release -p forge-app                   # teclea solo y registra frames lentos
FORGE_CONFIG=/tmp/prueba.toml cargo run --release -p forge-app      # usa otra configuración
```
