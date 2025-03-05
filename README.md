# FlashThing

FlashThing is a tool for quickly and easily flashing the Spotify Car Thing (Superbird). FlashThing is composed of three parts:

- **FlashThing**: Rust crate for flashing superbird.
- **FlashThing CLI**: Command line interface for FlashThing.
- **FlashThing Node**: N-API bindings for FlashThing.

FlashThing currently supports flashing the Stock partition tables as well as custom partition tables using a subset of the Terbium `meta.json` standard. Read more about that standard in the [docs](./docs/meta.md).

## Installation

### Rust Crate

```bash
cargo add flashthing
```

### CLI

```bash
cargo install flashthing-cli
```

### Node Module Installation

```bash
npm install flashthing
yarn add flashthing
pnpm add flashthing
bun add flashthing
```

### Platform Specific Notes

#### Linux

FlashThing requires `libusb` to be installed, and a udev rule must be set up to access the Car Thing. To install the udev rule, run the following command:

```bash
sudo flashthing-cli --udev
```

#### macOS

FlashThing requires `libusb` to be installed. You can install it using [Homebrew](https://brew.sh/):

```bash
brew install libusb
```

#### Windows

FlashThing may require special drivers (I don't have a Windows machine to test on). If you have issues, try running the [Terbium driver script](https://driver.terbium.app/get).

```powershell
irm https://driver.terbium.app/get | iex
```

## Usage

### Rust Crate Usage

See [docs.rs](https://docs.rs/flashthing/latest/flashthing/) and the [cli](./cli) for more information.

Note: The documentation is very basic, sorry!

### CLI Usage

```bash
❯ flashthing-cli --help
cli for flashing the Spotify Car Thing

Usage: flashthing-cli [OPTIONS] [PATH]

Arguments:
  [PATH]  Path to a zip file or a directory. Defaults to the current working directory if omitted

Options:
  -s, --stock    Whether the directory or archive contains a stock dump with no `meta.json` file
      --unbrick  Whether to unbrick the device
      --setup    setup host - this currently only sets up udev rules on Linux
  -h, --help     Print help
  -V, --version  Print version
```

### Node Module Usage

```typescript
import { FlashThing, type FlashEvent } from 'flashthing';

const callback = (event: FlashEvent) => {
  console.log('Flash event:', event);
};

const flasher = new FlashThing(callback);
await flasher.openArchive('path/to/archive.zip');

console.log(`Total flashing steps: ${flasher.getNumSteps()}`);
await flasher.flash();
```

## Project Structure

```bash
.
├── bindings # N-API bindings
├── cli # command line interface
└── lib # main library - has all the logic
```
