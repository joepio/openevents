# Profielen

Een profiel is een domein op het register. Het is data, geen code, en wordt tijdens runtime geregistreerd (eis F10).

## Wat een profiel bevat

```yaml
id: nl.vng.gesprekken
version: 0.1.0
subjectTypes:
  gesprek:
    uriTemplate: "{base}/gesprekken/{id}"
    queryable: [status, onderwerp, aanleiding.zaak, aanleiding.product, laatsteBijdrage.tijd, deelnames.partij]
    text: [onderwerp]
    references:
      aanleiding.zaak: { type: zaakspiegel }
      deelnames.partij: { external: true }
    sensitive: []
    retention: { followsReference: aanleiding.zaak }
eventTypes:
  nl.vng.gesprekken.gesprek.gestart:       { dataschema: ".../gesprek-gestart.json",   right: write }
  nl.vng.gesprekken.bijdrage.toegevoegd:   { dataschema: ".../bijdrage.json",          right: append }
  nl.vng.gesprekken.bijdrage.gelezen:      { dataschema: ".../gelezen.json",           right: append }
  nl.vng.gesprekken.deelname.toegevoegd:   { dataschema: ".../deelname.json",          right: write }
  nl.vng.gesprekken.gesprek.afgesloten:    { dataschema: ".../afgesloten.json",        right: append }
projection: ./projecties/gesprek.wasm      # of een declaratieve reducer, zie hieronder
rules: ./regels/gesprek.yaml          # read, append, write, fields; zie autorisatie.md
views: [mijn-open-gesprekken, werkvoorraad-gesprekken]
policies: [gesprekken-inwoner, gesprekken-medewerker]
```

## Projectieregels

Een projectie zet events om in de stand. Twee vormen:

1. **Declaratief** voor het gewone werk: "dit event zet veld X", "dit event voegt toe aan lijst Y", "dit event zet status op Z". Dat dekt het meeste en is makkelijk te controleren.
2. **Plugin** voor regels die meer vragen: een WASM-module met een vaste interface (`apply(state, event) -> (state, derivedEvents)`), met limieten op brandstof en geheugen. Dit is hetzelfde idee als `before_commit` in atomic-server.

Afgeleide events (bijvoorbeeld het automatische antwoord "binnen 2 werkdagen") worden in dezelfde transactie toegevoegd, met `source` gelijk aan het register en een kenmerk `automatic: true`.

Tijdsgestuurde regels (een gesprek sluit na 3 dagen zonder bijdrage van de gemeente) zijn een interne taak die het profiel declareert. De taak schrijft een gewoon event, zodat de geschiedenis compleet blijft.

## Versies

Een profiel heeft een versie. Een event noemt via `dataschema` de versie van zijn schema. Een nieuwe profielversie mag velden toevoegen; velden weghalen of betekenis veranderen vraagt een nieuw eventtype. Zo blijft het log over de jaren leesbaar.

## Eerste profiel: Gesprekken

Het eerste echte profiel wordt MijnGesprekken, omdat daar een werkende PoC ligt (OneGround-fork, Open Inwoner-fork, demo op DemoStack) en het fieldlab van 10 november de vraag opriep.

| In de Gesprekken API (lab PR #47) | In OpenEvents |
|---|---|
| `POST /gesprekken`, `POST /gesprekken/{id}/bijdragen` | events `gesprek.gestart`, `bijdrage.toegevoegd` |
| `GET /gesprekken?...` | view `werkvoorraad-gesprekken` of `/query` |
| `GET /events` (SSE) | `GET /events` met filter op het profiel |
| deelnames en rechten | projectie plus lees- en schrijfregels over `deelnames.partij` en `zaaktype.team.leden` |
| gelezen | event `bijdrage.gelezen`, projectieveld `ongelezen` per deelnemer |
| automatisch antwoord | afgeleid event in de projectieregel |
| automatisch sluiten na 3 dagen | tijdsgestuurde regel |
| klantcontact bij afsluiten | consumer die `gesprek.afgesloten` volgt en Open Klant aanroept |
| bijlagen | `dataref` naar het file-upload-pattern |
| bewaartermijn volgt de zaak | `retention.followsReference` |

**Voor de pilot** blijft het REST-contract van Gesprekken bestaan, want Roxit en Maykin verwachten dat. Er zijn dan twee routes, en de demo laat beide zien:

1. **Naast:** de Gesprekken API (OneGround-fork) publiceert zijn events naar OpenEvents. De medewerkerconsole en Open Inwoner lezen via OpenEvents.
2. **Erop:** een dunne REST-laag vertaalt het Gesprekken-contract naar events op OpenEvents. Dan is OpenEvents de bron en verdwijnt de eigen opslag van de Gesprekken API.

Als route 2 werkt, is de conclusie dat een domein als Gesprekken een profiel kan zijn in plaats van een register.
