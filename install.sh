#!/usr/bin/env bash
# octoPOS-admin — installer de WSL.
# Lo invoca install.ps1 desde Windows via WSLENV; no se corre a mano.
set -euo pipefail

INSTALL_DIR="/opt/octopos"
COMPOSE_FILE="$INSTALL_DIR/docker-compose.yml"
ENV_FILE="$INSTALL_DIR/.env"

step()    { printf '\033[36m==> %s\033[0m\n' "$*"; }
ok()      { printf '\033[32m    %s\033[0m\n' "$*"; }
warn()    { printf '\033[33m    %s\033[0m\n' "$*"; }
fail()    { printf '\033[31m!!! %s\033[0m\n' "$*" >&2; exit 1; }

require_env() {
  [[ -n "${!1:-}" ]] || fail "Falta la variable $1. Este script se lanza desde install.ps1."
}

for v in \
  ADMIN_RAW_BASE ADMIN_MONGO_USER ADMIN_MONGO_DB ADMIN_MONGO_PASSWORD \
  ADMIN_JWT_SECRET ADMIN_PLATFORM_URL; do
  require_env "$v"
done

# ─── Docker ────────────────────────────────────────────────────────────────

step 'Verificando Docker...'
if command -v docker >/dev/null 2>&1; then
  ok "Docker ya instalado: $(docker --version)"
else
  step 'Instalando Docker Engine...'
  curl -fsSL https://get.docker.com | sh
  ok "Docker instalado: $(docker --version)"
fi

# Enable systemd inside the WSL distro and persist Docker as a managed
# service. Without this:
#   - dockerd does not start automatically when the distro boots
#   - the only way to bring it up is `service docker start` from a
#     login shell, which never happens on a headless reboot
# We run the block whether Docker was installed fresh or already
# present so an existing Docker that was started ad-hoc (via
# `service docker start` in a previous session) gets promoted to a
# real systemd unit going forward. Idempotent on every re-execution.
if ! grep -q '^\[boot\]' /etc/wsl.conf 2>/dev/null; then
  step 'Habilitando systemd en /etc/wsl.conf...'
  printf '\n[boot]\nsystemd=true\n' >> /etc/wsl.conf
  warn 'systemd habilitado — la próxima vez que reinicies Windows entra en efecto.'
fi
if pidof systemd >/dev/null 2>&1; then
  step 'Habilitando docker en el arranque (systemctl enable)...'
  systemctl enable --now docker.service containerd.service 2>/dev/null || true
  ok 'Docker quedará vivo entre reboots vía systemd.'
else
  warn 'systemd aún no está activo en esta distro — `wsl --shutdown` desde Windows + relanzar para que tome efecto.'
  service docker start || true
fi

if ! docker compose version >/dev/null 2>&1; then
  fail 'Falta el plugin compose de Docker. Instalalo con apt o reejecutá el instalador oficial de Docker.'
fi
ok "Compose OK: $(docker compose version)"

# ─── Carpetas + archivos ──────────────────────────────────────────────────

step "Preparando $INSTALL_DIR..."
mkdir -p "$INSTALL_DIR"/{uploads,backups,data}
cd "$INSTALL_DIR"

step 'Bajando docker-compose.yml...'
curl -fsSL "$ADMIN_RAW_BASE/docker-compose.yml" -o "$COMPOSE_FILE"

if [[ -f "$ENV_FILE" ]]; then
  ok '.env ya existe — respetando los valores actuales.'
else
  step 'Escribiendo .env con secrets locales (license + branch los maneja el admin después)...'
  curl -fsSL "$ADMIN_RAW_BASE/.env.example" -o "$ENV_FILE"
  esc() { printf '%s' "$1" | sed -e 's/[\/&|]/\\&/g'; }
  sed -i "s|^MONGO_USER=.*|MONGO_USER=$(esc "$ADMIN_MONGO_USER")|"               "$ENV_FILE"
  sed -i "s|^MONGO_DB=.*|MONGO_DB=$(esc "$ADMIN_MONGO_DB")|"                     "$ENV_FILE"
  sed -i "s|^MONGO_PASSWORD=.*|MONGO_PASSWORD=$(esc "$ADMIN_MONGO_PASSWORD")|"   "$ENV_FILE"
  sed -i "s|^JWT_SECRET=.*|JWT_SECRET=$(esc "$ADMIN_JWT_SECRET")|"               "$ENV_FILE"
  sed -i "s|^PLATFORM_URL=.*|PLATFORM_URL=$(esc "$ADMIN_PLATFORM_URL")|"         "$ENV_FILE"
  # The license key + branch slug are captured by the OctoPOS Admin's
  # activation screen the first time it launches; nothing tenant-
  # specific lives in this .env. Auth between the local API and the
  # platform happens via the activation token (Ed25519-signed) — no
  # shared secret is distributed.

  chmod 600 "$ENV_FILE"
  ok '.env listo y protegido (chmod 600).'
fi

# ─── Imágenes + levantar stack ────────────────────────────────────────────

step 'Bajando imágenes (mongodb + api)...'
docker compose pull

step 'Levantando stack...'
docker compose up -d

# ─── Health checks ────────────────────────────────────────────────────────

step 'Esperando a MongoDB...'
for i in $(seq 1 60); do
  if docker compose exec -T mongodb mongosh --quiet --eval 'db.runCommand({ping:1}).ok' 2>/dev/null | grep -q 1; then
    ok 'MongoDB lista.'
    break
  fi
  [[ $i -eq 60 ]] && { warn 'MongoDB no respondió en 60s.'; warn "  docker compose -f $COMPOSE_FILE logs mongodb"; exit 1; }
  sleep 1
done

step 'Esperando a la API (puede tardar 2-3 min la primera vez)...'
# Pegamos a `/` — el API responde 200 (admin SPA) o al menos alguna
# respuesta HTTP cuando Nest levanto. Sin `-f`, curl devuelve exito
# ante cualquier codigo HTTP; solo falla si el socket esta cerrado.
for i in $(seq 1 180); do
  if curl -sS -o /dev/null http://localhost:3000/ 2>/dev/null; then
    ok 'API respondiendo en http://localhost:3000.'
    break
  fi
  [[ $i -eq 180 ]] && { warn 'API no respondió en 180s.'; warn "  docker compose -f $COMPOSE_FILE logs api"; exit 1; }
  sleep 1
done

step 'Listo — MongoDB + API corriendo dentro de WSL.'
