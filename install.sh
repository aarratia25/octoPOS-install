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
  ADMIN_JWT_SECRET ADMIN_PLATFORM_URL ADMIN_PLATFORM_API_KEY ADMIN_BRANCH_SLUG; do
  require_env "$v"
done

# ─── Docker ────────────────────────────────────────────────────────────────

step 'Verificando Docker...'
if command -v docker >/dev/null 2>&1; then
  ok "Docker ya instalado: $(docker --version)"
else
  step 'Instalando Docker Engine...'
  curl -fsSL https://get.docker.com | sh
  if [[ -f /etc/wsl.conf ]] && ! grep -q '^\[boot\]' /etc/wsl.conf; then
    printf '\n[boot]\nsystemd=true\n' >> /etc/wsl.conf
    warn 'Habilité systemd en /etc/wsl.conf — corré `wsl --shutdown` en Windows para aplicar.'
  elif [[ ! -f /etc/wsl.conf ]]; then
    printf '[boot]\nsystemd=true\n' > /etc/wsl.conf
    warn 'Creé /etc/wsl.conf con systemd habilitado — corré `wsl --shutdown` en Windows.'
  fi
  service docker start || true
  ok "Docker instalado: $(docker --version)"
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
  step 'Escribiendo .env con los datos del onboarding...'
  curl -fsSL "$ADMIN_RAW_BASE/.env.example" -o "$ENV_FILE"
  esc() { printf '%s' "$1" | sed -e 's/[\/&|]/\\&/g'; }
  sed -i "s|^MONGO_USER=.*|MONGO_USER=$(esc "$ADMIN_MONGO_USER")|"               "$ENV_FILE"
  sed -i "s|^MONGO_DB=.*|MONGO_DB=$(esc "$ADMIN_MONGO_DB")|"                     "$ENV_FILE"
  sed -i "s|^MONGO_PASSWORD=.*|MONGO_PASSWORD=$(esc "$ADMIN_MONGO_PASSWORD")|"   "$ENV_FILE"
  sed -i "s|^JWT_SECRET=.*|JWT_SECRET=$(esc "$ADMIN_JWT_SECRET")|"               "$ENV_FILE"
  sed -i "s|^PLATFORM_URL=.*|PLATFORM_URL=$(esc "$ADMIN_PLATFORM_URL")|"         "$ENV_FILE"
  sed -i "s|^PLATFORM_API_KEY=.*|PLATFORM_API_KEY=$(esc "$ADMIN_PLATFORM_API_KEY")|" "$ENV_FILE"
  sed -i "s|^BRANCH_SLUG=.*|BRANCH_SLUG=$(esc "$ADMIN_BRANCH_SLUG")|"             "$ENV_FILE"
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
