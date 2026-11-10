# Modules

Een module is een klein, open pakket met de logica van één domein: bezwaar, melden openbare ruimte, Wmo-hulpmiddelen, een parkeervergunning. Modules zijn de commandkant van de [CQRS-architectuur](architectuur.md#cqrs-openevents-is-de-leeskant). Ze beslissen wat er mag gebeuren en schrijven daarvoor events naar OpenEvents. OpenEvents zelf blijft de leeskant.

## Waarom modules en geen BPMN-zaaktypen

Zaaktypen in BPMN beschrijven de volgorde van stappen. Het domein zit daar niet in: welke gegevens een aanvraag heeft, welke regels gelden, hoe een termijn wordt verdaagd, welke brief eruit moet, welke koppeling nodig is. Dat wordt er in scripts, formulieren en koppelingen omheen gebouwd. Specialistische pakketten (Wmo en jeugd, vergunningen, belastingen) hebben dat domein wel in hun software. Dus winnen zij, ook al zijn het silo's met een eigen opslag, login, portaal en koppelingen.

Een module neemt het goede van beide:

| | BPMN-zaaktype | Specialistisch pakket | Module |
|---|---|---|---|
| Domeinlogica en regels | buiten het model, in scripts | in code | in code, met tests |
| Gegevensmodel | generiek (eigenschappen) | eigen | eigen, als profiel |
| Termijnen | timers in het proces | in code | timers in de module |
| Opslag, rechten, zoeken, portaal, KCC | van het zaaksysteem | eigen silo | gedeeld (OpenEvents en de rest van de basis) |
| Overstappen | proces opnieuw modelleren | migratie | module wisselen; data en geschiedenis blijven |
| Delen tussen gemeenten | export en import | licentie | open pakket met versie |

BPMN mag blijven waar het helpt: als plaatje van het proces, of als orkestratie binnen een module. Het is alleen niet meer het fundament.

## Wat een module bevat

```
bezwaar/
  module.yaml          # naam, versie, rechten die de module nodig heeft
  profiel.yaml         # eventtypes, schema's, projectie, lees- en schrijfregels, views
  schemas/             # JSON Schema per event en per command
  commands.wasm        # handle(command, context) -> commit | fout
  reacties.wasm        # on_event(event, context) -> commits ; on_timer(timer, context) -> commits
  formulieren/         # JSON Schema plus UI-hints per command, voor portaal en console
  tests/               # gegeven / bij / verwacht
```

1. **Profiel (data).** Wordt in OpenEvents geregistreerd. Zie [profielen](profielen.md).
2. **Commands.** Een command is een verzoek met een eigen schema, bijvoorbeeld `bezwaar.indienen` of `bezwaar.ontvankelijkheid-beoordelen`. De module krijgt het command en de actuele stand van het onderwerp. Ze antwoordt met een JSON Commit (een of meer events, met `previousCommit`) of met een fout in `problem+json`.
3. **Reacties.** Een module kan op events reageren (ook van andere modules) en timers zetten. Het antwoord is weer een commit.
4. **Formulieren.** Voor elk command dat een mens geeft, een JSON Schema met UI-hints. Een portaal of medewerkerconsole kan het scherm daaruit maken, zonder code per module.
5. **Tests.** Elke regel als scenario: gegeven deze events, bij dit command, verwacht deze events of deze fout. De tests zijn de specificatie, en een tweede implementatie in een andere taal kan ertegen draaien.

## De modulehost

Modules draaien in een **modulehost** naast OpenEvents. De host kan in dezelfde container zitten, zodat hosten simpel blijft, maar is een apart onderdeel. Zie [ADR-0007](adr/0007-modules-in-wasm.md).

De host:

- neemt commands aan op `POST /commands/{module}/{command}` (met het token van de gebruiker) en roept de module aan;
- volgt de stroom van OpenEvents met een eigen cursor per module en roept `on_event` aan;
- houdt timers bij en roept `on_timer` aan;
- schrijft het resultaat met de identiteit van de module (`actor()` is dan de module, met `act.sub` voor de gebruiker namens wie);
- probeert bij een 412 (iemand anders schreef eerder) het command opnieuw met de nieuwe stand, een paar keer.

Wat een module van de host krijgt:

| Host-functie | Wat |
|---|---|
| `stand(subject)` | de projectie van het onderwerp |
| `query(filter)` | een CQL2-query, uitgevoerd met de rechten van de gebruiker, niet die van de module |
| `nu()` | de tijd (zodat tests een vaste tijd kunnen geven) |
| `fetch(url)` | alleen naar adressen die `module.yaml` noemt; geheimen vult de host in, de module ziet ze nooit |
| `log(...)` | logging |

Wat een module niet krijgt: bestanden, netwerk buiten de lijst, geheimen, andere onderwerpen dan via `query`. Rekentijd en geheugen zijn begrensd. Een module is daardoor bijna een pure functie, en dus voorspelbaar en toetsbaar.

Modules kunnen in elke taal die naar WASM-componenten compileert: Rust, Go, C#, en JavaScript via QuickJS. De interface staat in WIT (WebAssembly Component Model). atomic-server heeft deze runtime al (wasmtime, QuickJS, fuel-limieten, `fetch` met allowlist en geheimen die de host invult); die ervaring en code zijn te hergebruiken.

## Voorbeeld: bezwaar

**Commands**

| Command | Door | Regels |
|---|---|---|
| `bezwaar.indienen` | inwoner, gemachtigde | verwijst naar een besluit; binnen 6 weken na bekendmaking (Awb 6:7), anders markeren als mogelijk niet-ontvankelijk |
| `bezwaar.gronden-aanvullen` | indiener | alleen zolang de hersteltermijn loopt |
| `bezwaar.verzuim-herstellen-vragen` | behandelaar | zet hersteltermijn (Awb 6:6) |
| `bezwaar.ontvankelijkheid-beoordelen` | behandelaar | uitkomst ontvankelijk of niet, met motivering |
| `bezwaar.hoorzitting-plannen` | behandelaar | stukken minstens een week vooraf ter inzage (Awb 7:4) |
| `bezwaar.verdagen` | behandelaar | eenmalig, ten hoogste 6 weken (Awb 7:10 lid 3) |
| `bezwaar.beslissen` | behandelaar met mandaat | gegrond, ongegrond, niet-ontvankelijk |
| `bezwaar.intrekken` | indiener | zolang er geen beslissing is |

**Events** (in het profiel): `bezwaar.ingediend`, `bezwaar.gronden-aangevuld`, `bezwaar.hersteltermijn-gezet`, `bezwaar.ontvankelijkheid-beoordeeld`, `bezwaar.hoorzitting-gepland`, `bezwaar.verdaagd`, `bezwaar.beslist`, `bezwaar.ingetrokken`, `bezwaar.termijn-overschreden`.

**Reacties en timers**

- Bij `bezwaar.ingediend`: timer op de beslistermijn (6 weken, of 12 weken met een adviescommissie, Awb 7:10).
- Bij `bezwaar.verdaagd`: timer verschuiven.
- Timer verlopen zonder `bezwaar.beslist`: `bezwaar.termijn-overschreden` (basis voor ingebrekestelling en dwangsom, Awb 4:17).
- Bij `bezwaar.beslist`: een consumer maakt de brief en een klantcontact.

**Lezen** (gewoon via OpenEvents): de indiener ziet zijn bezwaren, de behandelaar ziet bezwaren tegen besluiten van zaaktypen van zijn team, de werkvoorraad is een view gesorteerd op resterende termijn.

**Een test**

```yaml
scenario: verdagen kan maar een keer
gegeven:
  - bezwaar.ingediend:  { besluit: "https://zaken.example.nl/besluiten/1", tijd: "2026-11-01T10:00:00Z" }
  - bezwaar.verdaagd:   { weken: 6 }
bij:
  bezwaar.verdagen:     { weken: 2 }
verwacht:
  fout: { type: "verdagen-al-gebruikt" }
```

## Voorbeeld: melden openbare ruimte

- Command `melding.doen` met een locatie (GeoJSON-punt), categorie en foto's (`dataref`).
- Regel: binnen 25 meter en 7 dagen een open melding van dezelfde categorie? Dan wordt het een `melding.gekoppeld` aan die melding in plaats van een nieuwe. De module vraagt dat met `query("categorie = 'losse-tegel' AND status = 'open' AND s_dwithin(locatie, POINT(...), 25) AND t_after(gemeld, ...)")`.
- Reactie: bij `melding.gedaan` toewijzen aan het team van het gebied, met een query op de gebieden (`s_within`).
- Lezen: de kaart in het portaal is een view met `s_intersects(locatie, <kaartbeeld>)`, live via SSE.

Dit voorbeeld laat zien waarom de filtertaal ruimte moet kennen ([ADR-0006](adr/0006-filtertaal-cql2.md)).

## Delen en versies

- Een module heeft een versie (semver). Het log noemt bij elk event welke module en versie het schreef (`source` plus een kenmerk `module`).
- Een nieuwe versie die eventtypes toevoegt, is een minor-versie. Een versie die bestaande events anders projecteert, is major en vraagt een herprojectie, die het register vanuit het log kan doen.
- Modules worden verspreid als ondertekend pakket (bijvoorbeeld een OCI-artefact). Een gemeente kiest welke versie ze draait.
- Een gemeente kan een module aanpassen door een eigen versie te publiceren. Omdat de tests meekomen, ziet ze direct of haar wijziging een regel breekt.

## Wat dit betekent voor een zaaksysteem

Een zaaksysteem op deze basis is: OpenEvents (opslag, lezen, rechten), de modulehost, een set modules (een per domein of zaaktype), consumers (brieven, klantcontact, archief) en een ZGW-façade voor bestaande afnemers. Zie [architectuur](architectuur.md#een-zaaksysteem-op-openevents).
