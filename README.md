# bytesctl

Reserve a table at [Restaurant bytes, Chur](https://bytes-chur.ch/) from the terminal.
It talks to the same aleno reservation API as the booking widget on the website.
The widget's public API key and restaurant id aren't stored in the code: bytesctl reads them
from the booking widget on the restaurant's website on first use, caches them in
`~/.cache/bytesctl/widget-<restaurant>.json`, and fetches them again if aleno ever rejects the
cached key. Other restaurants that use aleno can be added, see [Restaurants](#restaurants).

## Installation

Needs a [Rust toolchain](https://rustup.rs/) (Rust 1.88 or newer):

```sh
cargo install --locked --git https://github.com/seagleNet/bytesctl
```

Run the same command with `--force` to update. From a local checkout, use `cargo install --path .`.

### Shell completions

`bytesctl completions <shell>` prints a completion script for `bash`, `fish`, `zsh`, `elvish` or
`powershell`. The script asks bytesctl on each Tab press, so install it once and it stays current
after updates. `cancel` and `edit` also complete the IDs of your upcoming reservations.

```sh
# bash
mkdir -p ~/.local/share/bash-completion/completions
bytesctl completions bash > ~/.local/share/bash-completion/completions/bytesctl

# fish
mkdir -p ~/.config/fish/completions
bytesctl completions fish > ~/.config/fish/completions/bytesctl.fish

# zsh: the directory must be in $fpath before compinit runs in ~/.zshrc
mkdir -p ~/.zfunc
bytesctl completions zsh > ~/.zfunc/_bytesctl
```

## Usage

```sh
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

## Restaurants

bytes is built in. Any other restaurant that takes reservations through aleno can be added with
`--url` pointing at the page that embeds the booking widget. bytesctl checks that the page has a
widget, then adds the restaurant to your config file and leaves the rest of the file untouched:

```sh
bytesctl restaurants add muster --url https://restaurant.example/ \
  --name "Restaurant Muster" --phone "+41 44 123 45 67"
bytesctl restaurants remove muster
```

`--name` and `--phone` are optional, and `--force` replaces an existing entry. `remove` refuses a
restaurant that has upcoming reservations, because they couldn't be cancelled with bytesctl
afterwards; `--force` overrides that too.

The result is a normal config section, which you can also write by hand:

```toml
[defaults]
restaurant = "muster"  # optional, instead of bytes

[restaurants.muster]
name = "Restaurant Muster"
url = "https://restaurant.example/"
phone = "+41 44 123 45 67"  # optional, shown when something has to be done by phone
```

```sh
bytesctl restaurants                                 # what you can book
bytesctl --restaurant muster slots 2026-11-16   # or set $BYTES_RESTAURANT
bytesctl --restaurant muster book -t 12:30 -p 2
```

`bookings` lists reservations from all restaurants, and `cancel` and `edit` go to the restaurant the
reservation was made at, so they don't need `--restaurant`. Pass it only with a raw secret that
isn't saved locally. `book` shows aleno's name for the restaurant before you confirm.

If a page embeds several widgets, bytesctl stops and lists their ids. Pick one with
`--restaurant-id` when adding it, or `restaurant_id = "…"` in its section. `menu` only works for bytes, because the parser
is specific to its PDF. Times are always Swiss local time.

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
