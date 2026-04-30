# octoPOS — Servidor central

Instalador del servidor central (MongoDB + API) para comercios en
Venezuela. Las cajas (POS) y el admin se instalan aparte con sus `.msi`.

> Contexto local asumido: electricidad inestable, internet intermitente,
> hardware difícil de reponer. UPS y red cableada son obligatorios.

Contenido:

1. [Hardware](#1-hardware)
2. [Sistema operativo](#2-sistema-operativo)
3. [Red e infraestructura](#3-red-e-infraestructura)
4. [Instalación](#4-instalación)
5. [Después](#5-después)
6. [Actualizaciones](#6-actualizaciones)
7. [Troubleshooting](#7-troubleshooting)

---

## 1. Hardware

Xeon es preferible si hay 5+ cajas o el servidor opera 24/7 (ECC RAM,
MTBF, I/O sostenido). Para 1–4 cajas en comercio chico, una PC i5/i7
sirve, asumiendo el riesgo de no tener ECC y repuestos limitados en
plaza.

### Xeon (5+ cajas, 24/7)

| Recurso | Mínimo | Recomendado |
|---|---|---|
| CPU | Xeon E-2300 4c | Xeon E-2300 6–8c |
| RAM | 16 GB ECC | 32 GB ECC |
| SSD (SO + Mongo) | 256 GB NVMe | 512 GB NVMe |
| HDD (backups) | 1 TB | 2 TB |
| Red | Gigabit cableado | Gigabit cableado |
| UPS | obligatorio | obligatorio |

### PC (1–4 cajas)

| Recurso | Mínimo | Recomendado |
|---|---|---|
| CPU | i5 9na gen | i7 9na gen+ |
| RAM | 16 GB | 16 GB |
| SSD | 256 GB NVMe | 256 GB NVMe |
| HDD | 1 TB | 1 TB |
| Red | Gigabit cableado | Gigabit cableado |
| UPS | obligatorio | obligatorio |

### Carga esperada

| Cajas | RAM | CPU |
|---|---|---|
| 1–2 | 4–5 GB | <10 % |
| 3–5 | 5–7 GB | 10–25 % |
| 6–10 | 7–10 GB | 20–40 % |
| 10+ | 10–14 GB | 30–60 % (Xeon) |

Mongo es lo que más RAM consume; reserva 8 GB+ para 5+ cajas.

---

## 2. Sistema operativo

Solo necesitas Windows instalado. El script se encarga de WSL2, Ubuntu
22.04, Docker, imágenes y arranque — **no abres Microsoft Store, no
instalas nada a mano**.

| SO | Soportado |
|---|---|
| Windows Server 2022 / 2025 | ✅ (recomendado para Xeon) |
| Windows 11 Pro | ✅ |
| Windows 10 Pro build ≥ 19041 | ✅ (EOL próximo) |
| Windows Server 2019 | ❌ |
| Windows 10/11 Home | ❌ |

---

## 3. Red e infraestructura

- Gigabit cableado entre servidor y POS. WiFi solo para periféricos.
- IP fija en el servidor (DHCP reserva o estática).
- Puerto 3000 abierto solo en LAN.
- UPS dimensionado para **30 min** mínimo (apagones suelen pasar los
  15 min). Idealmente protege también switch y router.
- Protector de voltaje aguas arriba del UPS.
- Internet **no es necesario en operación** — solo para sync con la
  plataforma cloud y updates.

### Backups

`/opt/octopos/backups/` (dentro de WSL) tiene dumps automáticos:

- Copia diaria a externo/NAS, idealmente fuera del local.
- Restore drill mensual.
- Sync cloud replica los backups si está activado.

---

## 4. Instalación

### 1. PowerShell como Administrador

Win → `powershell` → clic derecho → *Ejecutar como administrador*. La
ventana debe decir "Administrador" en el título.

### 2. One-liner

```powershell
iwr -useb https://raw.githubusercontent.com/aarratia25/octoPOS-install/main/install.ps1 | iex
```

### 3. Onboarding

Te pide:

- **DB**: usuario, nombre, password (enter = default; `S` = autogenerar).
- **JWT secret**: enter autogenera.
- **Plataforma**: API key y **nombre** de la sucursal (ej:
  `San Bernardino`). El script genera el slug solo (`san-bernardino`).
  La URL ya viene fija en `https://platform.octo-pos.net`.

Solo API key y nombre de sucursal son obligatorios; el resto se autogenera.

### Qué hace el script (tú no haces nada de esto)

1. Valida Windows + admin.
2. Instala WSL2 + Ubuntu 22.04. La primera vez puede pedir reinicio de
   Windows; reinicias y vuelves a ejecutar el one-liner.
3. Dentro de Ubuntu: instala Docker, escribe `/opt/octopos/.env` con
   tus respuestas, descarga las imágenes (Mongo + API) y las levanta.

~10–15 min la primera vez. Es reejecutable sin riesgo: lo que ya está
instalado lo saltea.

### Archivos generados

```
/opt/octopos/
  docker-compose.yml
  .env
  uploads/  backups/  data/
```

Desde Windows: `\\wsl$\Ubuntu-22.04\opt\octopos\`.

---

## 5. Después

1. Admin `.msi`: <https://github.com/aarratia25/octoPOS-admin/releases/latest>.
   Se conecta a `localhost:3000` y se activa contra la plataforma.
2. POS `.msi` en cada caja apuntando a la IP fija del servidor.

---

## 6. Actualizaciones

**El instalador corre una sola vez.** Baja la última versión disponible
al momento de instalar y queda corriendo. A partir de ahí, las
actualizaciones las maneja el admin solo:

1. Un cron interno consulta el repo de GitHub periódicamente y detecta
   nuevos releases.
2. La página **Updates** del admin lista las versiones disponibles.
3. Aprietas el botón → el admin hace `pull` + `up -d` por dentro con la
   versión elegida. Rollback igual con otro clic.

No tocas archivos, no entras a WSL, no reejecutás el instalador.

> Reejecutar el instalador no rompe nada (es reentrante), pero **no
> actualiza la API**: respeta el `.env` existente y se limita a
> verificar WSL/Docker. Para actualizar usa siempre la UI.

---

## 7. Troubleshooting

| Problema | Solución |
| --- | --- |
| WSL no está + pide reiniciar | Reinicia, repite el one-liner. |
| Ubuntu pide crear usuario | Crea el unix user y repite. |
| API no responde en `:3000` | `wsl docker compose -f /opt/octopos/docker-compose.yml logs api` |
| Mongo no levanta | Igual, con `mongodb`. |
| Regenerar `.env` | Borra `/opt/octopos/.env` en WSL y repite. |
