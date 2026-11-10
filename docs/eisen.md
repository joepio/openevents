# Eisen

Eisen staan hier als eigenschappen, niet als techniek. Een techniek wordt pas gekozen in een ADR, en alleen als middel om een eis te halen.

## Functioneel

| # | Eis | Klaar als |
|---|---|---|
| F1 | Bronnen kunnen events schrijven | Een CloudEvent of JSON Commit-transactie wordt atomair opgeslagen of geweigerd met een `problem+json`-fout |
| F2 | Schrijven is veilig te herhalen | Hetzelfde `source` + `id` van dezelfde aanroeper levert hetzelfde resultaat op, zonder dubbel event |
| F3 | Gelijktijdig schrijven gaat niet stilletjes mis | Een transactie met `previousCommit` die niet meer klopt, krijgt 412 |
| F4 | Clients kunnen live volgen | `GET /events` als SSE levert nieuwe events binnen een seconde na opslag |
| F5 | Volgen hervat na een onderbreking | Met `Last-Event-ID` komen precies de gemiste events, zonder gat en zonder dubbele |
| F6 | De actuele stand is op te vragen | Per resource is er een stand die gelijk loopt met het log |
| F7 | Krachtig bevragen | Filters op velden, paden over verwijzingen (ten minste 2 stappen), sorteren, cursor-paginering, velden kiezen en gerelateerde resources meenemen |
| F8 | Benoemde representaties | Een beheerder kan een view registreren (query-template met parameters) die clients bij naam aanroepen |
| F9 | Snapshot en stroom sluiten op elkaar aan | Een query geeft een `feedCursor`; volgen vanaf die cursor mist niets dat na de snapshot gebeurde |
| F10 | Domeinen zonder nieuwe build | Een profiel (types, handelingen, regels) is tijdens runtime te registreren |
| F11 | Strenge validatie | Elk event wordt bij binnenkomst gecontroleerd tegen zijn handeling en het schema van de resource; ongeldige events worden geweigerd |
| F12 | Geschiedenis per onderwerp | Alle events van één onderwerp zijn op volgorde op te halen (tijdlijn) |
| F13 | Bijlagen | Grote inhoud gaat via verwijzingen (`dataref`, file-upload-pattern), niet inline |

## Autorisatie

| # | Eis | Klaar als |
|---|---|---|
| A1 | Rechten per onderwerp | Per onderwerp is vast te leggen wie mag lezen, toevoegen en schrijven (bijvoorbeeld curator: lezen en schrijven; inwoner: lezen) |
| A2 | Rechten op basis van relaties en eigenschappen | Een medewerker ziet onderwerpen via team, zaaktype of rol, zonder dat rechten per onderwerp worden uitgeschreven; een teamwissel is één wijziging |
| A3 | Geen lekken via tellingen | Totalen, offsets en cursors verraden niets over onderwerpen die je niet mag zien |
| A4 | Velden afschermen | Velden waarvoor je geen recht hebt, staan niet in de representatie, ook niet in meegenomen resources of op de stroom |
| A5 | Geen injectie | Autorisatie werkt op de geparste query, nooit door tekst aan een query te plakken |
| A6 | Producenten en gebruikers | Zowel systemen (Open Zaak, OneGround) als personen kunnen schrijven, elk met eigen rechten |
| A7 | Externe identiteit | Tokens komen van een externe OIDC-provider; het register heeft geen eigen login |
| A8 | Geen persoonsgegevens in URL's | BSN's en andere gevoelige waarden staan alleen in bodies |

## Wissen en bewaren

| # | Eis | Klaar als |
|---|---|---|
| W1 | AVG-verwijdering | Na een wisverzoek is de inhoud van een onderwerp niet meer te herstellen, ook niet uit het log of uit backups |
| W2 | Bewaartermijnen | Een onderwerp kan een vernietigingsdatum krijgen, afgeleid van het bovenliggende object |
| W3 | Wissen is zichtbaar | Abonnees krijgen een tombstone-event zodat zij hun kopie ook kunnen wissen |

## Kwaliteit

| # | Eis | Klaar als |
|---|---|---|
| K1 | Duurzaam schrijven | Een bevestigde write gaat niet verloren bij een crash |
| K2 | Backups | Online backup zonder stilstand, en herstel naar een tijdstip |
| K3 | Beschikbaarheid | Er is een route naar failover (replica die het log volgt) |
| K4 | Snelheid | Query's op een index binnen 50 ms bij 10 miljoen events op één node; meten met een benchmark in het repo |
| K5 | Begrensde kosten | Elke query heeft een kostengrens (diepte, aantal waarden, resultaatgrootte); te dure query's krijgen een fout in plaats van een trage server |
| K6 | Eenvoudig te hosten | Eén binary of container, zonder verplichte externe database |
| K7 | Open | Open source, open standaarden (CloudEvents, JSON Schema, OpenAPI, AsyncAPI, OIDC) |
| K8 | Conformance | Een testset die elke andere implementatie van hetzelfde contract kan draaien |
