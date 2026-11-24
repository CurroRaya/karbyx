# forge

[![Licencia: GPL v3](https://img.shields.io/badge/licencia-GPL--3.0--o--posterior-blue.svg)](LICENSE)

**Tu agente de programación para macOS, con los modelos que tienes en casa.**

Sirves un modelo en tu PC con llama.cpp o Strata y forge, en el Mac, se conecta
a él por tu red: lee tu proyecto, lo edita y ejecuta comandos, pidiéndote
permiso. El modelo solo piensa; tus archivos se quedan en el Mac y lo que el
agente lee solo viaja a tu PC. Es una app nativa, escrita en Rust y dibujada
por la GPU, y está en español.

![El chat de forge: un cambio en un archivo, un comando no permitido y la respuesta del modelo](assets/chat.png)

*Captura hecha con un servidor de prueba: ni el modelo ni la velocidad son reales.*

| Vista | Qué es |
|---|---|
| **Chat** (⌘L) | El agente de forge con tus modelos locales. Es con lo que se abre. |
| **Mesa** (⌘E) | Para dirigir a la vez a Claude Code, Codex CLI y Gemini CLI, cada uno en su propia copia del proyecto. |
| **Terminal** | Una terminal GPU completa, con paneles, bloques de comandos y el consumo de tus asistentes. |

De momento tu modelo local trabaja en el chat, no en la mesa.

forge es software libre: puedes usarlo, estudiarlo, modificarlo y compartirlo.
Las versiones modificadas que se distribuyan deben publicar su código con la
misma licencia, para que todas las mejoras sigan siendo de todos.

## Empezar en cinco minutos

**1. Instala forge en el Mac** (necesitas [Rust y las herramientas de
Xcode](#requisitos)):

```bash
git clone https://github.com/CurroRaya/forge_term.git
cd forge_term
scripts/bundle.sh --install    # crea Forge.app y la copia a ~/Applications
```

**2. Abre forge** (⌘Espacio, «forge»), pulsa **Conectar con mi PC** y copia la
orden de tu servidor. Lleva una clave ya generada, que se rellena sola en el
formulario.

**3. En el PC**, pega la orden. Con llama.cpp, cambia `modelo.gguf` por la ruta
del tuyo:

```powershell
llama-server -m modelo.gguf --host 0.0.0.0 --port 8080 -c 32768 -ngl 99 --api-key CLAVE
```

```powershell
strata launch --access lan --port 8080 --api-key CLAVE
```

Si Windows pregunta por el cortafuegos, permite solo las redes privadas.

**4. Escribe la dirección del PC** en forge (`192.168.1.50:8080`) y pulsa ↵. En
Windows la da `ipconfig`: la «Dirección IPv4» del adaptador que tiene puerta de
enlace. La primera vez, macOS pide permiso para la red local.

**5. Elige una carpeta** y pide algo.

## Requisitos

- **macOS** (probado en macOS 27). Linux y Windows aún no están soportados: el
  PC del modelo puede ser cualquiera, pero forge se ejecuta en un Mac.
- **Un servidor de modelos** con la API de OpenAI. Probado con llama.cpp y
  Gemma 4 26B; Strata y otros deberían funcionar, pero no los hemos probado. El
  modelo debe admitir **herramientas** para trabajar en una carpeta, y se
  recomiendan **32k de contexto** o más.
- **Herramientas de línea de comandos de Xcode** (el enlazador que usa Rust):

  ```bash
  xcode-select --install
  ```

- **Rust 1.88 o superior** (se usa la edición 2024). Si no lo tienes, la forma
  recomendada es [rustup](https://rustup.rs):

  ```bash
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  ```

  O con Homebrew:

  ```bash
  brew install rustup && rustup default stable
  ```

  Homebrew instala rustup sin enlazarlo al sistema, así que hay que añadirlo
  al `PATH` (y `~/.cargo/bin`, donde se instalan los programas de Rust):

  ```bash
  echo 'export PATH="/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH"' >> ~/.zshrc && source ~/.zshrc
  ```

- **Opcional:** [Claude Code](https://claude.com/claude-code), Codex CLI o
  Gemini CLI para la mesa, sus sesiones y el panel de consumo.

## Instalación

### Como app de macOS (recomendado)

```bash
scripts/bundle.sh --install    # crea Forge.app y la copia a ~/Applications
```

Crea `target/release/Forge.app`, con su icono y firmada para este Mac, y con
`--install` la copia a `~/Applications`. Desde ahí la abres con Spotlight
(⌘Espacio, «forge») o la anclas al Dock. Sin `--install` solo la crea.

Abierta desde el Dock, sus terminales empiezan en tu carpeta personal y toman
el idioma de macOS (`LANG`), como Terminal.

### Desde la terminal

```bash
cargo install --path crates/forge-app      # la app          → ~/.cargo/bin/forge-app
cargo install --path crates/forge-usage    # CLI de consumo  → ~/.cargo/bin/forge-usage
```

La primera compilación tarda unos minutos; las siguientes, segundos.

## Ejecutar

Una vez instalado, desde cualquier terminal:

```bash
forge-app                 # abre forge
forge-usage               # resumen de consumo de tus asistentes
forge-usage watch         # consumo en directo
forge-usage -p claude daily
```

Sin instalar, desde la carpeta del proyecto:

```bash
cargo run --release -p forge-app
cargo run --release -p forge-usage -- watch
```

Si lo lanzas desde una terminal y quieres poder cerrarla sin que se cierre
forge, lánzalo desvinculado (o usa la [app](#como-app-de-macos-recomendado)):

```bash
forge-app &!
```

## Actualizar

```bash
cd forge_term
git pull
scripts/bundle.sh --install                # si usas la app
cargo install --path crates/forge-app      # si la instalaste con cargo
cargo install --path crates/forge-usage
```

Cierra forge antes y vuelve a abrirla después.

## Desinstalar

```bash
cargo uninstall forge-app forge-usage
rm -rf ~/Applications/Forge.app                  # si instalaste la app
rm -rf ~/.config/forge ~/Library/Caches/forge    # configuración y caché (opcional)
rm -rf ~/Library/Application\ Support/forge      # conversaciones, servidores y claves (opcional)
```

forge no modifica tu `.zshrc` ni tus ajustes (solo edita lo que le permites en
la carpeta que elijas). La integración con zsh
vive en `~/Library/Caches/forge` y desaparece con ella.

## Solución de problemas

| Problema | Solución |
|---|---|
| «No llego a…» al conectar | El PC está en otra red, o forge no tiene permiso en *Ajustes del Sistema › Privacidad y seguridad › Red local*. |
| «… no responde» al conectar | El PC está apagado o el cortafuegos de Windows corta el puerto. Comprueba también la IP: el router puede cambiarla. |
| «… no hay nada en el puerto 8080» | El servidor no está arrancado, usa otro puerto o solo escucha en el propio PC: falta `--host 0.0.0.0` (llama.cpp) o `--access lan` (Strata). |
| «El servidor pide una clave» | La clave de forge no es la de `--api-key`. Menú de modelos › *Editar*. |
| El modelo contesta, pero no usa las herramientas | Elige una carpeta; si ya la tienes, ese modelo o su plantilla no las admiten. |
| Olvida cosas en tareas largas | El contexto se llena: arranca el servidor con más (`-c`) o empieza otra conversación (⌘N). |
| `zsh: command not found: cargo` | Rust no está en el `PATH`; ver [Requisitos](#requisitos) y abre una terminal nueva. |
| `command not found: forge-app` tras instalar | Falta `~/.cargo/bin` en el `PATH` (misma línea de arriba). |
| `linker 'cc' not found` al compilar | Instala las herramientas de Xcode: `xcode-select --install`. |
| No aparecen los bloques de comandos | Solo funcionan con zsh y la integración activa (`shell_integration = true`). Abre una pestaña nueva tras cambiarlo. |
| El panel de consumo no sale | Aparece solo en pestañas con una sesión de IA; fuérzalo con ⌘⌥B. |
| Va lento o quieres medirlo | `FORGE_PROFILE=1 forge-app` muestra el tiempo de cada frame. |

## Estructura del proyecto

| Crate | Qué hace |
|---|---|
| `forge-agent` | El agente, sin interfaz: cliente del servidor de modelos (streaming y cancelación), bucle de herramientas con permisos, y el sandbox de macOS con la ejecución de comandos |
| `forge-app` | La app: render por GPU (wgpu + glyphon), chat, mesa, terminales con pestañas y paneles, panel de consumo, `forge.toml` con temas y atajos |
| `forge-term` | Sesión de terminal independiente de la UI: PTY con bucle de E/S propio + emulador VT (`alacritty_terminal`), bloques OSC 133 e integración con zsh, selección, búsqueda, protocolos de ratón y snapshot del grid |
| `forge-ai` | Lee el historial de Claude Code (`~/.claude`), Codex CLI (`~/.codex`) y Gemini CLI (`~/.gemini/tmp`), lo normaliza, deduplica y calcula ventanas de 5 h, semana, proyecciones y límites oficiales cuando el proveedor los publica (Codex) |
| `forge-usage` | CLI de consumo: `summary`, `daily`, `blocks`, `models`, `projects`, `watch`, `line`, `--json`, `-p claude\|codex\|gemini` |

## Uso

### El chat (⌘L)

- **Carpeta**: elígela abajo a la izquierda y el modelo podrá leer, buscar,
  editar y ejecutar en ella. Sin carpeta es un chat normal, sin herramientas.
  Una conversación empezada se queda en su carpeta. Si el proyecto tiene
  `AGENTS.md` (o `CLAUDE.md`), el modelo lo lee al empezar.
- **Modelo**: el menú junta los de todos tus servidores. Si el tuyo se apaga,
  forge lo reintenta solo. Desde ahí se abre también **Claude Code**, en una
  terminal de forge y en la misma carpeta, con tu suscripción y sus permisos.
- **Permisos**: leer dentro de la carpeta es libre; editar y ejecutar se
  preguntan enseñando el diff o el comando. **⌘↵** permite, **esc** para, y si
  escribes otra cosa el modelo la recibe como respuesta.
- **Automático** (⇧⇥, o *Siempre en este chat*): edita en la carpeta y ejecuta
  en el sandbox sin preguntar. Trabaja sobre tus archivos, no sobre una copia:
  úsalo con git al día.
- **Mientras trabaja** puedes escribir el siguiente mensaje, que se envía al
  acabar; **esc** lo para. Puedes tener varias conversaciones a la vez, y forge
  avisa si una termina o pide permiso y no la estás viendo.
- **Conversaciones**: se guardan solas a cada paso y siguen ahí al reabrir
  forge, ordenadas por fecha. Si lo cierras con una a medias, se para y puedes
  continuarla. Al reabrir, todas vuelven a preguntar antes de cambiar nada. La ×
  (dos clics) la borra.
- **Contexto**: abajo a la derecha se ve cuánto llevas usado y la velocidad.
  Cerca del límite, el agente olvida los resultados antiguos de sus
  herramientas.

| Herramienta | Qué hace | Permiso |
|---|---|---|
| `read`, `list`, `search`, `find` | Leer, listar, buscar en el contenido y por nombre | Libre dentro de la carpeta |
| `edit`, `write` | Cambiar una parte de un archivo, o crearlo | Se pregunta, con el diff |
| `bash` | Ejecutar un comando en la carpeta, dentro del sandbox | Se pregunta |

**Se pregunta siempre**, también en Automático: ejecutar fuera del sandbox,
salir de la carpeta, leer archivos que suelen tener claves (`.env`, `*.pem`...)
y escribir en `.claude`, `.mcp.json`, `AGENTS.md` y similares, que otras
herramientas leen. En `.git` no se puede escribir con `edit` ni `write`.

#### Qué protege y qué no

- Los comandos se ejecutan en el **sandbox de macOS**: sin red, sin lanzar
  otras apps, sin leer las claves habituales (`~/.ssh`, `~/.aws`, llavero,
  `~/.claude`...) y escribiendo solo en la carpeta, una caché de compilación y
  una carpeta temporal. Si no se puede aislar, no se ejecutan. El resto de tus
  archivos sí se puede leer, y un comando puede leer el `.env` del proyecto.
- La conexión con el PC es **HTTP sin cifrar**: la conversación y la clave
  viajan en claro. Úsala en tu red o por una VPN, y nunca abras el puerto a
  internet. Pon siempre una clave: sin ella, cualquiera de tu red puede usar el
  modelo.
- Los servidores y sus claves se guardan en
  `~/Library/Application Support/forge/chat.json`, que solo tú puedes leer.
- Las **conversaciones** se guardan sin cifrar en
  `~/Library/Application Support/forge/chat.d`, también solo para ti, con lo que
  el modelo haya leído de tus archivos. Borrarlas en forge las borra del disco.

### La mesa (⌘E)

Para trabajar con Claude Code, Codex CLI y Gemini CLI. Cada agente tiene
temperatura: los que trabajan laten como brasas, el que te espera está al rojo
y lo terminado se enfría a acero.

- **«¿Qué hay que hacer?»**: escribe la tarea y pulsa ↵. forge lanza un agente
  (el asistente seleccionado, Claude por defecto) en un **worktree propio**
  del proyecto elegido, con la tarea ya escrita. ⇥ cambia de proyecto (los de
  tus terminales abiertas). Fuera de un repositorio git, el agente trabaja en
  la carpeta, sin worktree.
- **Columnas**: *Trabajando*, *Te necesita*, *Para revisar* y *Hecho hoy*.
  Cada tarjeta muestra la tarea, el proyecto y la rama, lo que hace ahora,
  una **miniatura en vivo de su terminal** (un minimapa con los colores
  reales: se ve si está escribiendo código, pasando tests o con un diff en
  pantalla), su última línea, sus `+/−` y sus tokens.
- **Responder sin entrar**: cuando un agente te pide permiso («¿Aplico este
  cambio en login.rs?»), su tarjeta muestra la pregunta y un botón por opción
  (*Sí*, *Sí, siempre*, *No*). Un clic, o la tecla 1-3 con la tarjeta
  seleccionada, responde en su terminal. forge vuelve a leer la pantalla antes
  de enviar nada: si la pregunta ya no está, no responde.
- **Verificación automática**: cuando un agente termina en su worktree, forge
  puede ejecutar los tests en segundo plano, sin gastar tokens, y la tarjeta
  llega a *Para revisar* con un sello: **✓ 36 tests en verde** o **✕ 2 tests
  fallan** (o *No compila*). Viene desactivada: la primera vez forge pregunta
  **¿Verifico?** y eliges *Una vez* o *Siempre* (en ese proyecto). Con un
  fallo, **Que lo arregle** le pasa al agente la salida completa; ↻ repite y
  **Verificar** la lanza a mano (p. ej. con Codex o Gemini, que no avisan de
  cuándo terminan). El comando se detecta solo (`cargo test`,
  `npm`/`pnpm`/`yarn`/`bun test`, `go test`, `pytest`, `make test`) o se
  configura en `[verify]`.
  - **Aislada**: los tests son código que ha podido escribir el agente y que
    aún no has revisado, así que se ejecutan siempre en el sandbox de macOS:
    sin red, sin lanzar otras apps, sin leer las claves habituales (`~/.ssh`,
    `~/.aws`, llavero, `~/.claude`...) y escribiendo solo en el worktree, la
    caché de compilación y una carpeta temporal. Si no se puede aislar, no se
    ejecuta. El resto de tus archivos sí se puede leer.
  - Si el agente ha cambiado algo que decide qué se ejecuta (`Makefile`,
    `build.rs`, `Cargo.toml`, `package.json`...), forge lo dice y espera a que
    pulses *Verificar*, aunque el proyecto esté activado.
  - Si tus tests necesitan red (o `localhost`): `[verify] network = ["proyecto"]`.
    Con red, un test podría enviar fuera lo que puede leer.
  - Los proyectos activados están en `~/Library/Caches/forge/verify-projects.txt`,
    uno por línea: borra la línea para desactivarlo.
- Clic en una tarjeta (o ↑↓ y ↵) abre su terminal; **Revisar** abre el diff
  (⌘R) para fusionar o descartar. Abajo, tus terminales normales.
- **⌘E** alterna entre la mesa y las terminales; ⌘T, ⌘⇧C... también te llevan
  a las terminales.

### Atajos

| Atajo | Acción |
|---|---|
| ⌘L · ⌘N | El chat / conversación nueva |
| ⌘E | La mesa de agentes / volver a las terminales |
| ↵ / ⇧↵ | Enviar / línea nueva (en el chat) |
| ⌘↵ / esc | Permitir lo que pide el modelo / no permitirlo y parar |
| ⇧⇥ | Preguntar antes / Automático (en el chat) |
| ⌘T / ⌘⇧C | Nueva pestaña de terminal / del asistente seleccionado |
| ⌘D / ⌘⇧D | Dividir el panel a la derecha / abajo |
| ⌘⌥←→↑↓ | Moverse entre paneles |
| ⌘W | Cerrar el panel activo (o la pestaña si es el último) |
| ⌘1…⌘9, ⌃Tab, ⌘⇧↑/↓ | Cambiar de pestaña |
| ⌘F, ⌘G / ⌘⇧G | Buscar en el historial, siguiente / anterior (↵ y ⇧↵ dentro de la barra) |
| ⌘C / ⌘V | Copiar la selección / pegar (con bracketed paste) |
| ⌘B / ⌘⌥B | Mostrar u ocultar la barra de sesiones / el panel de consumo (este aparece solo en pestañas con una sesión de IA; ⌘⌥B lo fuerza en la pestaña actual) |
| ⌘+ / ⌘- / ⌘0 | Tamaño de fuente |
| ⌘← / ⌘→, ⌥← / ⌥→, ⌘⌫ | Inicio / fin de línea, palabra anterior / siguiente, borrar línea |
| ⌘↑ / ⌘↓ | Seleccionar el comando (bloque) anterior / siguiente; Esc quita la selección |
| ⌘C (con un bloque seleccionado) | Copiar el comando y su salida |
| ⌘I | Preguntar a la IA por el bloque seleccionado o el último que falló |
| ⌘J | Bandeja de agentes: los que te esperan primero; ↑↓ y ↵ para ir a uno |
| ⌘U | Panel de mando: consumo de todos los asistentes y agentes abiertos |
| ⌘⇧A | Agente nuevo en su propio worktree de git |
| ⌘R | Revisar cambios: diff del agente (fusionar ↵, descartar ⌫) o de la terminal |
| ⌘K | Limpiar pantalla |
| ⌘Q | Salir |

**Ratón:** arrastrar selecciona, doble clic selecciona una palabra, triple
clic la línea y ⌥+arrastrar un bloque rectangular. Los divisores entre
paneles se arrastran. Si la aplicación pide el ratón (vim, htop, tmux) los
clics le llegan a ella; con ⇧ se fuerza la selección normal.

### Agentes externos: forge sabe qué hacen

Con Claude Code, Codex CLI y Gemini CLI, en la mesa o en una terminal:

- **Estado en la barra lateral:** cada sesión de un agente muestra si está
  **trabajando** (◐), **te espera** (● naranja) o ha **terminado** (✓), con su
  tarea como título, lo que hace ahora («Editando ui.rs», «Ejecutando: cargo
  test») y desde cuándo.
- **Agrupadas por proyecto** (la raíz del repositorio git de cada sesión).
- **Bandeja de atención (⌘J):** todos los agentes de todas las pestañas, los
  que te esperan primero. El aviso «● N te esperan» de la cabecera la abre.
- **Avisos de macOS** cuando un agente te espera o termina y no lo estás viendo.
- **Panel de mando (⌘U):** una tarjeta por asistente (hoy, ventana de 5 h,
  semana y las últimas 24 h por horas), el histórico de 14 días apilado por
  asistente, los agentes abiertos con los tokens que lleva cada sesión (Claude)
  y los proyectos que más consumen esta semana.

Con **Claude Code** el estado es exacto: forge lo lanza con
`--settings` y unos *hooks* que le cuentan cada paso (también si escribes
`claude` a mano en una terminal de forge). Tus ajustes de Claude no se tocan.
Con otros agentes (Codex, Gemini...) se deduce de su actividad.

### Los agentes ven tus terminales (MCP)

forge es también un servidor [MCP](https://modelcontextprotocol.io): un
agente que corre dentro de forge puede ver las otras terminales sin que
copies nada. Pídele, por ejemplo, «mira el error de la otra terminal» o «¿por
qué ha fallado lo último que ejecuté?». Herramientas, todas de **solo
lectura** (un agente nunca escribe en tus terminales):

| Herramienta | Qué hace |
|---|---|
| `list_terminals` | Terminales abiertas: pestaña, proyecto, carpeta y estado de cada agente |
| `read_terminal` | Últimas líneas de una terminal, tal como las ves |
| `list_commands` | Últimos comandos con su código de salida y duración |
| `read_command` | Un comando con su salida completa (por defecto el último; `failed` = el último que falló) |

Sin indicar terminal, usan la que tienes delante o la última que usaste.
**Claude Code** lo trae configurado: forge lo lanza con `--mcp-config` y
permite estas herramientas sin preguntar (compruébalo con `/mcp`). Para otros
clientes MCP, añade un servidor stdio con el comando `forge-app mcp`; solo
funciona dentro de una terminal de forge.

### Un worktree por agente y revisión de cambios

Para que varios agentes trabajen en el mismo proyecto sin pisarse, **⌘⇧A**
abre el agente en su **propio worktree de git**: una copia del repositorio en
`<proyecto>.worktrees/agente-N`, junto al proyecto, con su rama
`forge/agente-N` creada desde el último commit (lo que tengas sin commit no
se lleva). La barra lateral lo marca con `⎇ agente-N`.

**⌘R** abre el panel **Revisar cambios** del panel activo: la lista de
archivos tocados con sus `+/−` y el diff de cada uno, que se actualiza solo
mientras el agente trabaja. En un agente con worktree:

- **↵ Fusionar** guarda en un commit lo pendiente (con la tarea del agente
  como mensaje) y lo fusiona en la rama actual del proyecto. Si hay
  conflictos no se toca nada y te lo dice.
- **⌫ Descartar** (dos veces, para confirmar) cierra el agente y borra su
  worktree y su rama.

En cualquier otra terminal dentro de un repositorio, ⌘R muestra lo que hay
sin commit. Si cierras el agente sin fusionar, el worktree se queda: abre una
terminal en esa carpeta y ⌘R lo vuelve a encontrar.

### Bloques de comandos (estilo Warp)

Con zsh, forge activa sola una integración con la shell (no toca tu
`.zshrc`): cada comando y su salida forman un **bloque**.

- Un separador fino entre comandos; los que fallan llevan una barra y un tinte
  rojos y la etiqueta `✗ código`; los que tardan muestran su duración (`✓ 2,4 s`).
- Clic en el margen izquierdo o ⌘↑/⌘↓ para seleccionar un bloque; ⌘C copia
  el comando con su salida.
- En un bloque fallido aparece **◆ Preguntar a Claude** (o al asistente
  activo): abre al lado una sesión con el comando, el error y la salida ya
  escritos.
- Si un comando largo termina mientras forge no está a la vista, llega un
  aviso de macOS.

Las marcas son el estándar OSC 133, así que también funcionan con otras
shells que las emitan (bash con su propia integración, fish, starship...).

### Configuración (`~/.config/forge/forge.toml`)

Se aplica al guardar, sin reiniciar:

```toml
[font]
family = "Menlo"
size = 14
line_height = 1.3

[window]
padding = 10
start = "chat"             # o "mesa" o "terminal": qué se ve al abrir

[terminal]
scrollback = 10000
option_as_alt = false      # true: ⌥ envía Meta (ESC+tecla) en vez de @, #, ñ...
shell_integration = true   # bloques de comandos (zsh)
notify_after_secs = 10     # aviso al terminar comandos largos (0 = nunca)

[theme]
name = "forja"             # forja (carbón y brasa) | one-dark | dracula | solarized-dark | gruvbox | light
# background = "#171412"   # cualquier color suelto sobrescribe el tema

[verify]                   # tests de un agente, en un sandbox
auto = true                # solo en los proyectos que actives; false: en ninguno
network = []               # proyectos cuyos tests necesitan red
timeout_secs = 600
[verify.commands]          # por proyecto (nombre de la carpeta); "off" la desactiva
forge = "cargo test --workspace"

[keys]                     # redefine atajos; "none" desactiva
split_right = "cmd+d"
find = "cmd+f"
```

Acciones configurables: `copy`, `paste`, `quit`, `new_tab`, `new_ai`, `close`,
`next_tab`, `prev_tab`, `toggle_sidebar`, `toggle_usage`, `font_bigger`,
`font_smaller`, `font_reset`, `clear`, `split_right`, `split_down`,
`focus_left`, `focus_right`, `focus_up`, `focus_down`, `find`, `find_next`,
`find_prev`, `block_prev`, `block_next`, `ask_ai`, `inbox`, `dashboard`,
`new_agent`, `review`, `board`, `chat`, `new_chat`.

### Asistentes y límites

El panel derecho de consumo aparece solo cuando la pestaña activa tiene una
sesión de IA (abierta con ⌘⇧C o un `claude`/`codex`/`gemini` lanzado a mano) y
muestra la vista de ese asistente. Por defecto forge detecta qué asistentes
tienes instalados y muestra una pestaña por cada uno en ese panel. Se puede elegir en
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
cargo run --release -p forge-agent --example charla -- 192.168.1.50:8080 /ruta/proyecto "tarea"  # el agente sin interfaz; ejecuta en Automático
FORGE_SCREENSHOT=/tmp/forge.png cargo run --release -p forge-app   # vuelca un frame a PNG
FORGE_PROVIDER=codex cargo run --release -p forge-app               # arranca con ese asistente
FORGE_SELFTEST=1 cargo run --release -p forge-app                   # teclea solo y registra frames lentos
FORGE_PROFILE=1 FORGE_SELFTEST=1 cargo run --release -p forge-app  # registra el tiempo de todos los frames
FORGE_CONFIG=/tmp/prueba.toml cargo run --release -p forge-app      # usa otra configuración
FORGE_CAPTURE_AI=1 FORGE_SCREENSHOT=/tmp/f.png cargo run --release -p forge-app  # captura con una pestaña de IA simulada
FORGE_CAPTURE_AI=real FORGE_SCREENSHOT=/tmp/f.png cargo run --release -p forge-app  # captura con Claude de verdad (sin escribirle: no gasta tokens)
FORGE_CAPTURE_INBOX=1 FORGE_CAPTURE_AI=1 FORGE_SCREENSHOT=/tmp/f.png cargo run --release -p forge-app  # captura con la bandeja ⌘J
FORGE_CAPTURE_DASH=1 FORGE_SCREENSHOT=/tmp/f.png cargo run --release -p forge-app  # captura con el panel de mando ⌘U
FORGE_CAPTURE_CHAT="1500:hola" FORGE_CHAT_STATE=/tmp/chat.json FORGE_SCREENSHOT=/tmp/f.png cargo run --release -p forge-app  # el chat
FORGE_CAPTURE_BOARD=1 FORGE_CAPTURE_LAUNCH="tarea" FORGE_CAPTURE_AI=1 FORGE_SCREENSHOT=/tmp/f.png cargo run --release -p forge-app  # la mesa, lanzando una tarea
FORGE_CAPTURE_WORKTREE=1 FORGE_CAPTURE_AI=1 FORGE_CAPTURE_REVIEW=1 FORGE_SCREENSHOT=/tmp/f.png cargo run --release -p forge-app  # agente en worktree y panel ⌘R
echo '{"hook_event_name":"Stop"}' | FORGE_SOCKET=~/Library/Caches/forge/sock/<pid>.sock FORGE_PANE=1 forge-app hook  # simula un hook
FORGE_TYPE=$'ls\nfalse\n' FORGE_CAPTURE_SELECT=1 FORGE_CAPTURE_DELAY=7000 FORGE_SCREENSHOT=/tmp/f.png cargo run --release -p forge-app  # captura con bloques
```

## Contribuir

Las contribuciones son bienvenidas: fallos, ideas, documentación o código.
Lee [CONTRIBUTING.md](CONTRIBUTING.md) antes de abrir un *pull request*.

## Licencia

Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de forge.

Este programa es software libre: puedes redistribuirlo y/o modificarlo bajo
los términos de la **Licencia Pública General de GNU** publicada por la Free
Software Foundation, ya sea la versión 3 de la licencia o (a tu elección)
cualquier versión posterior.

Se distribuye con la esperanza de que sea útil, pero **SIN NINGUNA
GARANTÍA**; ni siquiera la garantía implícita de COMERCIABILIDAD o IDONEIDAD
PARA UN PROPÓSITO PARTICULAR. Consulta el archivo [LICENSE](LICENSE) para más
detalles.

