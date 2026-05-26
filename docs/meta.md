# Superbird Flash Configuration Format

The meta.json file follows the Terbium flash standard, which defines a structured way flash the Spotify Car Thing.

## Metadata Versions

| Version | Description                                                                           |
| ------- | ------------------------------------------------------------------------------------- |
| 1       | Targets the Amlogic MPT partition table via named-partition steps.                    |
| 2       | Adds the `writeBootPartition` and `writeUserArea` steps for whole-image GPT flashing. |

Version 2 is a strict superset: every version 1 configuration is also a valid version 2 configuration. The new steps exist for mainline u-boot images, where the firmware is a single GPT disk image written to the eMMC user area plus a signed bootloader written to the boot hwpartitions, rather than a set of named MPT partitions.

## Basic Structure

```json
{
  "name": "Example Firmware",
  "version": "1.0.0",
  "description": "This is an example firmware configuration",
  "steps": [
    // Array of steps to execute
  ],
  "variables": {
    // Optional variables (currently useless)
  },
  "metadataVersion": 2
}
```

## Top-Level Fields

| Field           | Type   | Required | Description                                     |
| --------------- | ------ | -------- | ----------------------------------------------- |
| name            | string | Yes      | Name of the firmware configuration              |
| version         | string | Yes      | Version of the firmware configuration           |
| description     | string | Yes      | Description of the firmware configuration       |
| steps           | array  | Yes      | Array of steps to execute during flashing       |
| variables       | object | No       | Variables to store data between steps           |
| metadataVersion | number | Yes      | Version of the metadata format (must be 1 or 2) |

Variables are currently useless since FlashThing doesn't hand control back to the caller.

## Steps

Each step in the `steps` array must have a `type` property that determines the operation to perform.

### Supported Step Types

| Step Type            | Description                                   | Parameters                                                                        |
| -------------------- | --------------------------------------------- | --------------------------------------------------------------------------------- |
| `bulkcmd`            | Execute a bulk command                        | `value`: string                                                                   |
| `run`                | Execute code at a memory address              | `value`: object with `address` and optional `keepPower`                           |
| `writeSimpleMemory`  | Write data to memory                          | `value`: object with `address` and `data`                                         |
| `writeLargeMemory`   | Write large data to **DISK** (misnomer)       | `value`: object with `address`, `data`, `blockLength`, and optional `appendZeros` |
| `writeAMLCData`      | Write AMLC data                               | `value`: object with `seq`, `amlcOffset`, and `data`                              |
| `bl2Boot`            | Boot using custom BL2 (happens automatically) | `value`: object with `bl2` and `bootloader`                                       |
| `restorePartition`   | Restore a partition                           | `value`: object with `name` and `data`                                            |
| `writeBootPartition` | Write a boot hwpartition wholesale (v2)       | `value`: object with `hwpart` and `data`                                          |
| `writeUserArea`      | Write a span of the user area at an LBA (v2)  | `value`: object with `lba` and `data`                                             |
| `writeEnv`           | Write to the environment                      | `value`: string or file reference                                                 |
| `log`                | Log a message                                 | `value`: string                                                                   |
| `wait`               | Wait for specified time                       | `value`: object with `type: "time"` and `time` in milliseconds                    |

### Unsupported Step Types

These step types are defined in the standard but are currently not supported by Flashthing:

| Step Type                       | Description                           |
| ------------------------------- | ------------------------------------- |
| `identify`                      | Identify the device                   |
| `bulkcmdStat`                   | Execute a bulk command and get status |
| `readSimpleMemory`              | Read from memory                      |
| `readLargeMemory`               | Read large data from memory           |
| `getBootAMLC`                   | Get boot AMLC information             |
| `validatePartitionSize`         | Validate partition size               |
| `wait` with `type: "userInput"` | Wait for user input                   |

This is because FlashThing doesn't hand control back to the caller.

## Version 2 Steps

These steps require `metadataVersion` 2. They flash a mainline-style GPT image directly to the eMMC, bypassing the Amlogic MPT named-partition model used by the version 1 steps.

### writeBootPartition

Writes a payload wholesale to one of the eMMC boot hwpartitions. The bytes are staged into DDR in a single transfer, then written at LBA 0 of the selected hwpartition, after which the user area (hwpartition 0) is reselected. Used to place the signed bootloader on `boot0` and `boot1`.

| Field    | Type       | Required | Description                                      |
| -------- | ---------- | -------- | ------------------------------------------------ |
| `hwpart` | number     | Yes      | Boot hwpartition index: `1` = boot0, `2` = boot1 |
| `data`   | DataOrFile | Yes      | Payload to write                                 |

```json
{
  "type": "writeBootPartition",
  "value": {
    "hwpart": 1,
    "data": { "filePath": "superbird-boot.bin" }
  }
}
```

### writeUserArea

Streams a payload onto the user area (hwpartition 0) starting at an absolute LBA, chunked with progress reporting. The sector size is 512 bytes, so the byte offset of the write is `lba * 512`. Used to write the GPT disk image at LBA 0 and to splice additional partition images (such as the daemon overlay) at their fixed LBAs.

| Field  | Type       | Required | Description                                             |
| ------ | ---------- | -------- | ------------------------------------------------------- |
| `lba`  | number     | Yes      | Absolute LBA on the user area; sector size is 512 bytes |
| `data` | DataOrFile | Yes      | Payload to write                                        |

```json
{
  "type": "writeUserArea",
  "value": {
    "lba": 0,
    "data": { "filePath": "superbird.wic" }
  }
}
```

## Data Formats

### DataOrFile

Many steps accept a `DataOrFile` parameter, which can be either:

1. An array of bytes (integers)
2. A file reference object:

```json
{
  "filePath": "./path/to/file.bin",
  "encoding": "optional-encoding"
}
```

### StringOrFile

Some steps accept a `StringOrFile` parameter, which can be either:

1. A simple string
2. A file reference object (same format as above)

## Variable Substitution

Variables can be referenced in string values using the `${variableName}` syntax. Variables are used in next steps to make use of data from previous steps. Variables are not supported at this time.

## Example Configurations

### Version 1 (named-partition flash)

```json
{
  "name": "Example Firmware",
  "version": "1.0.0",
  "description": "This is an example firmware",
  "steps": [
    {
      "type": "writeLargeMemory",
      "value": {
        "address": 0,
        "data": { "filePath": "bootfs.bin" },
        "blockLength": 4096
      }
    },
    {
      "type": "writeEnv",
      "value": { "filePath": "env.txt" }
    },
    {
      "type": "log",
      "value": "Flash completed successfully"
    }
  ],
  "metadataVersion": 1
}
```

### Version 2 (whole-image GPT flash)

```json
{
  "name": "Example Mainline Firmware",
  "version": "1.0.0",
  "description": "Mainline u-boot whole-image flash",
  "steps": [
    { "type": "bulkcmd", "value": "amlmmc key" },
    {
      "type": "writeBootPartition",
      "value": { "hwpart": 1, "data": { "filePath": "superbird-boot.bin" } }
    },
    {
      "type": "writeBootPartition",
      "value": { "hwpart": 2, "data": { "filePath": "superbird-boot.bin" } }
    },
    {
      "type": "writeUserArea",
      "value": { "lba": 0, "data": { "filePath": "superbird.wic" } }
    }
  ],
  "metadataVersion": 2
}
```
