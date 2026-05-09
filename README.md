# OctoPOS — Instalador

Instalador del servidor central para una tienda. Las cajas (POS) se
instalan aparte con su propio `.msi`.

> Contexto local asumido: electricidad inestable, internet intermitente,
> hardware difícil de reponer. UPS y red cableada son obligatorios.

---

## 1. Hardware

Xeon es preferible si hay 5+ cajas o el servidor opera 24/7 (ECC RAM,
MTBF, I/O sostenido). Para 1–4 cajas en comercio chico, una PC i5/i7
sirve, asumiendo el riesgo de no tener ECC y repuestos limitados.

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

Mongo es lo que más RAM consume; reservá 8 GB+ para 5+ cajas.

---

## 2. Sistema operativo

| SO | Soportado |
|---|---|
| Windows Server 2022 / 2025 | ✅ (recomendado para Xeon) |
| Windows 11 Pro | ✅ |
| Windows 10 Pro build ≥ 19041 | ✅ (EOL próximo) |
| Windows Server 2019 | ❌ |
| Windows 10/11 Home | ❌ |

---

## 3. Red

- Gigabit cableado entre servidor y POS. WiFi solo para periféricos.
- IP fija en el servidor (DHCP reserva o estática).
- UPS dimensionado para 30 min mínimo. Idealmente protege también
  switch y router.
- Internet **no es necesario en operación** — solo durante la
  instalación, validación inicial de licencia y descarga de updates.

---

## 4. Instalación

1. Abrí <https://github.com/aarratia25/octoPOS-releases/releases/latest>
   y bajá **`OctoPOS-Setup-vX.Y.Z.exe`** (el que empieza con
   `OctoPOS-Setup-`). Es lo único que tenés que descargar — los demás
   `.exe` / `.msi` los baja el instalador solo en runtime desde el
   mismo release.

2. Doble-click. Aceptás el UAC una vez.

3. Esperá. El instalador muestra un splash con barra de progreso y
   hace todo solo: WSL2 + Ubuntu + Docker + Mongo + API + admin.
   Tarda 10–15 minutos. Si Windows pide reiniciar, el instalador
   reanuda solo después del reboot.

4. Cuando termina, el OctoPOS Admin abre solo. Ahí pide la **clave
   de licencia** y la **sucursal** una sola vez — es el único momento
   en que un humano interactúa. La activación valida contra la
   plataforma y queda guardada en la máquina.

5. El instalador se borra solo de "Programas y características" después
   de terminar — sólo queda **OctoPOS Admin** en la lista.

---

## 5. Actualizaciones

Las maneja el admin solo. El operador hace click en **"Actualización
disponible"** (botón verde en el sidebar) → barra de progreso →
listo. No se vuelve a abrir este instalador.

---

## 6. Si algo falla

| Problema | Acción |
|---|---|
| SmartScreen bloquea el `.exe` | Click "Más información" → "Ejecutar de todas formas". |
| Pide reiniciar Windows | Reiniciá. El instalador continúa solo después del reboot. |
| El admin rechaza la clave o la sucursal | Verificá con el proveedor que la licencia esté ACTIVA y la sucursal exista. |
| Cualquier otro error | Compartí con el proveedor el archivo `%ProgramData%\OctoPOS\setup.log`. |
