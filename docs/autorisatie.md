# Autorisatie

**Autorisatie is een query.** Je mag zien wat een filter over jou oplevert. Er is geen aparte beslisser, geen aparte beleidstaal en geen beslissing per rij. Het register gebruikt voor rechten dezelfde filtertaal en dezelfde engine als voor [`/query`](query.md).

In de gangbare termen is het een mix van ABAC (regels op eigenschappen) en ReBAC (regels die een relatie naar jou volgen, zoals Google Zanzibar). Het verschil met klassieke ABAC (XACML, OPA) is dat de relaties als data in het register zelf staan, en dat een regel een filter is. Daardoor draait hij als query, in plaats van dat er per rij een beslissing wordt gevraagd.

## Regels in het profiel

Per type staan in het profiel een leesregel, een schrijfregel en eventueel velden per rol:

```yaml
gesprek:
  read: >
    deelnames.partij = actor()
    OR aanleiding.zaak.zaaktype.team.leden = actor()
  append: >
    deelnames.partij = actor()
  write: >
    rollen.curator = actor() OR rollen.bewindvoerder = actor() OR rollen.behandelaar = actor()
    OR aanleiding.zaak.zaaktype.team.leden = actor()
  fields:
    - when: "actor('rol') = 'kcc'"
      show: [onderwerp, status, laatsteBijdrage.tijd]
```

De regels zijn [CQL2](query.md#filtertaal)-tekst, dezelfde taal als een query. Ze worden bij het registreren van het profiel geparst tot CQL2-JSON.

- `actor()` is de identiteit uit het token (een URI van een persoon, medewerker of systeem). `actor('rol')` is een claim uit het token.
- `read` bepaalt welke onderwerpen je ziet: in query's, in views, op de stroom en in de tijdlijn.
- `append` bepaalt of je events mag toevoegen van types die het profiel als "toevoegen" markeert (zoals `bijdrage.toegevoegd`).
- `write` bepaalt of je alle types mag schrijven (zoals `gesprek.afgesloten` of `deelname.toegevoegd`).
- Een pad door een lijst matcht als *een* element matcht, per vergelijking apart. `deelnames.partij = actor() AND deelnames.rol = 'curator'` kan dus waar zijn met de partij uit de ene deelname en de rol uit de andere. Daarom houdt de projectie een veld `rollen` bij (`rollen.curator` is de lijst partijen met die rol), en kijkt de schrijfregel daarnaar. Zie `examples/profiles/gesprekken.json`.
- `fields` bepaalt welke velden je ziet. Zonder regel zie je alle velden van onderwerpen die je mag lezen.

## Rechten zijn relaties, en relaties zijn data

De kracht zit in de paden. Teams, zaaktypen en lidmaatschappen zijn gewone onderwerpen in het register, met een eigen klein profiel (`nl.vng.organisatie`): een team heeft `leden`, een zaaktype verwijst naar een `team`. De leesregel loopt via een pad naar `actor()`.

Zo zijn alle gevallen uit MijnGesprekken één regel:

| Wie | Waarom die het ziet |
|---|---|
| inwoner | is deelnemer (`deelnames.partij`) |
| curator, bewindvoerder | is deelnemer met die rol, en mag daarom ook schrijven |
| behandelaar | zit in het team van het zaaktype van de zaak |
| KCC-medewerker | zit in het KCC-team, ziet alleen onderwerp en status (`fields`) |

Van buiten komt alleen de identiteit. Wie lid is van welk team, staat in het register, en wijzigt via events zoals alles. Een gemeente die dat al in een ander systeem bijhoudt (een IAM of FTV), laat dat systeem die events publiceren.

Vertegenwoordiging werkt hetzelfde. Een gemachtigde staat als deelname met rol `gemachtigde` in het gesprek, of het token draagt zowel de vertegenwoordigde als de gemachtigde (`sub` en `act.sub` uit RFC 8693), en de regel kijkt naar beide.

## Hoe het in de engine werkt

1. De query van de client wordt geparst. Variabelen zijn daar niet toegestaan.
2. De leesregel van het type wordt geparst bij het registreren van het profiel, en bij elke query wordt `actor()` ingevuld.
3. De engine voert `filter van de client AND leesregel` uit, als één boom, met dezelfde indexen en grenzen. Dat gebeurt vóór paginering, dus tellingen, offsets en cursors lekken niets.
4. De veldregel bepaalt de representatie, ook van elke `include`. Een verborgen veld ziet eruit als een veld zonder waarde.
5. Filteren, sorteren en zoeken op een veld dat je niet mag zien, geeft dezelfde fout als een onbekend veld. Hangt de veldregel af van het onderwerp (bijvoorbeeld "andere inwoners zien een melding zonder melder"), dan ziet het filter per onderwerp alleen de getoonde velden. Zo kun je met een filter niet raden wat er in een verborgen veld staat.

Er wordt nooit tekst aan een query geplakt. Een regel kan niets uitdrukken wat de query-taal niet kan, en valt dus onder dezelfde grenzen.

**Uitlegbaar:** "waarom zie ik dit gesprek?" is een vraag die het register kan beantwoorden door de takken van de regel apart te draaien. Dat is ook de basis voor logging volgens de standaard Logboek Dataverwerkingen.

## Op de stroom

Een SSE-verbinding gebruikt dezelfde leesregel. Het register houdt per open stroom bij welke onderwerpen erin vallen. Verandert dat, door een nieuw event op het onderwerp of doordat jij uit een team gaat, dan stuurt het `subject.matched` of `subject.unmatched`, en vanaf dat moment komt er wel of niets meer van dat onderwerp.

Een verlopen token sluit de stroom niet hard af. De server stuurt `auth.expiring`, de client haalt een nieuw ticket en gaat verder met `Last-Event-ID`.

## Schrijven

Een event mag als het onderwerp vóór het event door je `append`- of `write`-regel komt. Bij een nieuw onderwerp telt de stand ná het event: je kunt geen gesprek starten waarin je zelf niet zou mogen schrijven. Een weigering is altijd één 403 zonder details, zodat je niet kunt aftasten welke onderwerpen bestaan (zoals in ZaakChat #28).

Systemen zoals Open Zaak of OneGround zijn ook een `actor()`. Het profiel geeft hun rechten per `source`: OneGround mag `nl.vng.gesprekken.*` schrijven met `source` gelijk aan zijn eigen URI. Alleen de bron die een onderwerp heeft gemaakt, mag het wissen (lab ADR-0004).

## Centraal beheer

Regels zijn data. Ze staan in het profiel en zijn te lezen via `GET /profiles`. Als VNG, de G4 of een gemeente regels centraal wil beheren (zoals met FTV), kan een beheertool ze daar ophalen, tonen en via `PUT /profiles/{id}` uitrollen. Het register hoeft daarvoor niets extra's te kunnen.

Een externe beslisser (AuthZEN, OPA) is daarom niet nodig. Wil een gemeente toch voor bepaalde schrijfacties een centrale ja/nee-vraag stellen, dan kan dat later als optionele haak in het schrijfpad. Voor lezen en bevragen nooit, want daar breekt het de eigenschappen hierboven.
