# Beheer

## Backup en herstel

- `OPENEVENTS_BACKUP_DIR`: map voor backups. OpenEvents schrijft dan elke `OPENEVENTS_BACKUP_HOURS` uur (standaard 24) de hele opslag naar `openevents-{tijd}.jsonl`, in één leestransactie, dus consistent terwijl de server draait. De laatste zeven blijven staan.
- `OPENEVENTS_RESTORE`: pad naar zo'n bestand. Bij het opstarten laadt OpenEvents het in, maar alleen als de opslag leeg is (een nieuwe datamap). Daarna start alles zoals gewoonlijk, inclusief het opnieuw opbouwen van indexen als profielen veranderd zijn.
- Een backup bevat alles, ook persoonsgegevens. Geef de map dezelfde bescherming als de datamap, en houd de bewaartermijn van backups kort: verwijderen (zie [wissen.md](wissen.md)) werkt pas door in backups als de oude backups weg zijn.
- Het hele bestand gaat door het geheugen. Dat is prima tot een paar GB; daarboven is een stromende export nodig.

## Monitoring

- `GET /health` geeft `ok` als de opslag antwoordt, anders 503. Geschikt als healthcheck in Docker of Kubernetes.
- `GET /metrics` in Prometheus-formaat, zonder persoonsgegevens:

| Metriek | Waar je op let |
|---|---|
| `openevents_events_total` | groeit het log nog? |
| `openevents_last_event_timestamp_seconds` | wanneer kwam het laatste event? |
| `openevents_notificaties_total{outcome}` | `failed` hoort niet te groeien; Open Notificaties probeert opnieuw |
| `openevents_last_notificatie_timestamp_seconds` | stilte terwijl er wel zaken veranderen = abonnement kwijt |
| `openevents_abonnement_checked_timestamp_seconds` | elke 5 minuten bijgewerkt als het abonnement bij Open Notificaties klopt |
| `openevents_last_backup_timestamp_seconds` | ouder dan een dag = backup mislukt |
| `openevents_stream_clients` | open SSE-verbindingen |
| `openevents_purged_total` | verwijderd door bewaartermijn of `DELETE` |
