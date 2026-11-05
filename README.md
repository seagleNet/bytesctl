# bytesctl

Reserve a table at [Restaurant bytes, Chur](https://bytes-chur.ch/) from the terminal.
It talks to the same aleno reservation API as the booking widget on the website.
The widget's public API key and restaurant id aren't stored in the code: bytesctl reads them
from the booking widget on the homepage on first use, caches them in
`~/.cache/bytesctl/widget.json`, and fetches them again if aleno ever rejects the cached key.

```sh
cargo install --path .

bytesctl calendar --month 2026-11          # open / closed days
bytesctl slots 2026-11-10 -p 4             # free times for 4 guests (add --all for full ones)
bytesctl book -d 2026-11-10 -t 19:00 -p 2 \
  --first-name Max --last-name Muster \
  --email you@example.com --phone +41791234567 \
  --comment "Fensterplatz bitte" --dry-run  # drop --dry-run to actually book
bytesctl bookings                          # upcoming reservations made with bytesctl (--all for past/cancelled)
bytesctl bookings --refresh                # ask aleno for the current status of each reservation
bytesctl edit Ab12Cd -p 4                  # change number of guests (also -d DATE, -t TIME, --comment)
bytesctl edit Ab12Cd -t 12:30 --dry-run    # only ask aleno whether the change is possible
bytesctl cancel Ab12Cd                     # cancel by ID (or unique prefix) from `bookings`
bytesctl cancel                            # no ID needed if there is exactly one upcoming reservation

bytesctl menu                              # this week's lunch menu (Mittagskarte), today marked
bytesctl menu --today                      # only today's lunch
bytesctl menu --all                        # plus seasonals, bowls and toppings
```

The lunch menu PDF is found via the link on the bytes-chur.ch homepage; `--url <pdf>` or
`--file <pdf>` override that, `--print-url` just prints the link.

Every reservation is recorded in `~/.local/share/bytesctl/bookings.json` (respects
`$XDG_DATA_HOME`) together with its cancellation secret, so you never need to keep the secret
yourself. The file is created with mode 0600; `bookings --show-secrets` prints the secrets.
`cancel <secret>` still accepts a raw secret for reservations made elsewhere.

The status column shows the status from booking time until you run `--refresh`, then aleno's
latest state: `open` (registered, not explicitly accepted — aleno uses the same state for
provisional requests), `accepted`, `reconfirmed`, `cancelled`, `waiting list`, `moved`, … A
reservation aleno no longer knows shows as `not found`; changed times or party sizes are updated.

`book` asks aleno for the new reservation's status right away and warns you if it can't be
cancelled online (a same-day booking couldn't be, for example). aleno only reports yes or no, not
the deadline, so bytesctl can't warn before you book.

`cancel` checks with aleno first and stops early if the reservation is already cancelled or can no
longer be cancelled online. This uses the booking widget's undocumented `popup/info` endpoint, so
the less common states in the mapping are unverified.

`edit` uses aleno's guest self-edit (the "change reservation" link in the confirmation email):
it first asks aleno whether the change is possible, shows old → new, and only applies it after
confirmation. Values you don't pass stay as they are. Changes that need extra questions, a
credit-card guarantee or a group arrangement are refused with a pointer to the email link or
the restaurant's phone number. Like `popup/info`, the self-edit endpoints are undocumented.

The PDF is cached in `~/.cache/bytesctl/` (respects `$XDG_CACHE_HOME`), so repeated calls are
instant and work offline. The cache is used until the menu's week is over; after that the website
is re-checked (at most hourly). `bytesctl menu --refresh` forces a fresh download.

## Config file

```sh
bytesctl config init    # writes ~/.config/bytesctl/config.toml (respects $XDG_CONFIG_HOME)
bytesctl config show    # effective defaults
bytesctl config path
```

```toml
[defaults]
date = "today"        # "today", "tomorrow" or YYYY-MM-DD
time = "11:45"        # Swiss local time
people = 2
lang = "de"
# comment = "Fensterplatz bitte"
newsletter = false

[contact]
first_name = "Max"
last_name = "Muster"
email = "you@example.com"
phone = "+41791234567"
salutation = "male"   # male | female | other | prefer-not-say

[menu]
all = true            # also show seasonals/bowls/toppings (override: --no-all)
today = false         # only today's lunch (override: --week)
```

With contact details in the config, `bytesctl book` alone books today at 11:45 for 2.
Precedence: command-line flag > `BYTES_*` env var (`BYTES_FIRST_NAME`, `BYTES_LAST_NAME`,
`BYTES_EMAIL`, `BYTES_PHONE`, `BYTES_SALUTATION`) > config file > built-in default.
Use `--config <path>` or `$BYTES_CONFIG` for a different file.

`book` asks for confirmation unless `-y/--yes` is given. Shifts that need a credit-card
guarantee or extra questions are refused with a pointer to the website.
