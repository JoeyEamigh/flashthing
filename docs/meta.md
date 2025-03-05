# Superbird Flash Configuration Format

The meta.json file follows the Terbium flash standard, which defines a structured way flash the Spotify Car Thing.

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
  "metadataVersion": 1
}
```

## Top-Level Fields

| Field           | Type   | Required | Description                                |
| --------------- | ------ | -------- | ------------------------------------------ |
| name            | string | Yes      | Name of the firmware configuration         |
| version         | string | Yes      | Version of the firmware configuration      |
| description     | string | Yes      | Description of the firmware configuration  |
| steps           | array  | Yes      | Array of steps to execute during flashing  |
| variables       | object | No       | Variables to store data between steps      |
| metadataVersion | number | Yes      | Version of the metadata format (must be 1) |

Variables are currently useless since FlashThing doesn't hand control back to the caller.

## Steps

Each step in the `steps` array must have a `type` property that determines the operation to perform.

### Supported Step Types

| Step Type           | Description                                   | Parameters                                                                        |
| ------------------- | --------------------------------------------- | --------------------------------------------------------------------------------- |
| `bulkcmd`           | Execute a bulk command                        | `value`: string                                                                   |
| `run`               | Execute code at a memory address              | `value`: object with `address` and optional `keepPower`                           |
| `writeSimpleMemory` | Write data to memory                          | `value`: object with `address` and `data`                                         |
| `writeLargeMemory`  | Write large data to **DISK** (misnomer)       | `value`: object with `address`, `data`, `blockLength`, and optional `appendZeros` |
| `writeAMLCData`     | Write AMLC data                               | `value`: object with `seq`, `amlcOffset`, and `data`                              |
| `bl2Boot`           | Boot using custom BL2 (happens automatically) | `value`: object with `bl2` and `bootloader`                                       |
| `restorePartition`  | Restore a partition                           | `value`: object with `name` and `data`                                            |
| `writeEnv`          | Write to the environment                      | `value`: string or file reference                                                 |
| `log`               | Log a message                                 | `value`: string                                                                   |
| `wait`              | Wait for specified time                       | `value`: object with `type: "time"` and `time` in milliseconds                    |

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

## Example Configuration

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
