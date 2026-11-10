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
projection: ./projecties/gesprek.yaml      # declaratief, zie hieronder
rules: ./regels/gesprek.yaml          # read, append, write, fields; zie autorisatie.md
views: [mijn-open-gesprekken, werkvoorraad-gesprekken]
policies: [gesprekken-inwoner, gesprekken-medewerker]
```

## Projectieregels

Een projectie zet events om in de stand, **declaratief**: "dit event zet veld X", "dit event voegt toe aan lijst Y", "dit event zet status op Z", "dit event telt op bij teller W". Projectie is puur. Hij maakt geen nieuwe events, kent geen tijd en roept niets buiten het register aan. Dat houdt de kern klein, maakt een projectie altijd opnieuw af te spelen, en past bij CQRS: het register is de leeskant (zie [architectuur](architectuur.md)).

Past iets niet in de declaratieve vorm, dan is het meestal een domeinbeslissing en hoort het bij de commandkant.

## Reacties horen bij consumers

Regels die iets laten gebeuren, zijn geen onderdeel van het profiel maar kleine consumers naast het register. Ze volgen de stroom met een eigen cursor en schrijven gewone events terug:

- het automatische antwoord "binnen 2 werkdagen" op een bijdrage van een inwoner, met `automatic: true`
- een gesprek sluiten na 3 dagen zonder bijdrage van de gemeente
- een klantcontact in Open Klant aanmaken bij afsluiten

Een consumer is een gewone `$actor` met een eigen schrijfregel. Zo blijft de geschiedenis compleet, en is elk automatisch event te herleiden tot wie het schreef.

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
| automatisch antwoord | consumer die op `bijdrage.toegevoegd` reageert |
| automatisch sluiten na 3 dagen | consumer met een timer |
| klantcontact bij afsluiten | consumer die `gesprek.afgesloten` volgt en Open Klant aanroept |
| bijlagen | `dataref` naar het file-upload-pattern |
| bewaartermijn volgt de zaak | `retention.followsReference` |

**Voor de pilot** blijft het REST-contract van Gesprekken bestaan, want Roxit en Maykin verwachten dat. Er zijn dan twee routes, en de demo laat beide zien:

1. **Naast:** de Gesprekken API (OneGround-fork) publiceert zijn events naar OpenEvents. De medewerkerconsole en Open Inwoner lezen via OpenEvents.
2. **Erop:** een dunne REST-laag vertaalt het Gesprekken-contract naar events op OpenEvents. Dan is OpenEvents de bron en verdwijnt de eigen opslag van de Gesprekken API.

Als route 2 werkt, is de conclusie dat een domein als Gesprekken een profiel kan zijn in plaats van een register.
