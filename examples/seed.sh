#!/usr/bin/env bash
# Demo data for a local OpenEvents in dev mode (OPENEVENTS_DEV_AUTH=true).
# Usage: examples/seed.sh [http://localhost:8080]
set -euo pipefail
BASE="${1:-http://localhost:8080}"
SUZANNE="dev:https://id.example.nl/suzanne|rol=inwoner"
MERVE="dev:https://id.example.nl/merve|rol=medewerker|team=wmo"
LUKAS="dev:https://id.example.nl/lukas|rol=curator"
KAREL="dev:https://id.example.nl/karel|rol=inwoner"

post() {
  curl -fsS "$BASE/events" -H "authorization: Bearer $1" -H 'content-type: application/json' -d "$2" > /dev/null
}
ev() { # id type subject data
  printf '{"specversion":"1.0","id":"%s","source":"urn:seed","type":"%s","subject":"%s","data":%s}' "$1" "$2" "$3" "$4"
}

G1=urn:uuid:0192f7a0-0000-7000-8000-000000000001
G2=urn:uuid:0192f7a0-0000-7000-8000-000000000002
G3=urn:uuid:0192f7a0-0000-7000-8000-000000000003
P=nl.vng.gesprekken

post "$SUZANNE" "$(ev s1 $P.gesprek.gestart $G1 '{"onderwerp":"Vraag over mijn traplift","team":"wmo","aanleiding":{"zaak":"https://zaken.demostack.nl/zaken/ZAAK-2026-0012"},"initiatiefnemer":{"partij":"https://id.example.nl/suzanne","rol":"inwoner"}}')"
post "$SUZANNE" "$(ev s2 $P.bijdrage.toegevoegd $G1 '{"tekst":"Wanneer komt de monteur langs?"}')"
post "$MERVE" "$(ev s3 $P.bijdrage.toegevoegd $G1 '{"tekst":"Volgende week dinsdag tussen 9 en 12 uur.","automatisch":false}')"
post "$MERVE" "$(ev s4 $P.deelname.toegevoegd $G1 '{"partij":"https://id.example.nl/lukas","rol":"curator"}')"
post "$LUKAS" "$(ev s5 $P.bijdrage.toegevoegd $G1 '{"tekst":"Ik ben er dan ook bij."}')"
post "$MERVE" "$(ev s6 $P.gesprek.gestart $G2 '{"onderwerp":"Aanvullende stukken huishoudelijke hulp","team":"wmo","initiatiefnemer":{"partij":"https://id.example.nl/merve","rol":"behandelaar"}}')"
post "$KAREL" "$(ev s7 $P.gesprek.gestart $G3 '{"onderwerp":"Afvalpas kwijt","initiatiefnemer":{"partij":"https://id.example.nl/karel","rol":"inwoner"}}')"
post "$KAREL" "$(ev s8 $P.gesprek.afgesloten $G3 '{"reden":"Vraag beantwoord"}')"
post "$SUZANNE" '{"specversion":"1.0","id":"s9","source":"urn:seed","type":"json.commit.transaction","subject":"urn:notities","data":{"commits":[{"resourceId":"urn:notitie:1","type":"nl.example.notitie","resourceData":{"titel":"Boodschappen","eigenaar":"https://id.example.nl/suzanne","label":"thuis","lezers":["https://id.example.nl/lukas"]}},{"resourceId":"urn:notitie:2","type":"nl.example.notitie","resourceData":{"titel":"Vragen voor de consulent","eigenaar":"https://id.example.nl/suzanne","label":"wmo"}}]}}'
echo "seeded $BASE"
