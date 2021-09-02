# quartz_proxy
A proxy for logging packets between a minecraft client and server

## Compiling
This proxy relies on having a local clone of [Quartz](https://github.com/rusty-quartz/quartz) in the same parent directory the proxy, as it relies on `quartz_net` and `quartz_chat` to get the packet list and pretty print the data<br>


## Usage
1. Have a minecraft server running on `localhost:25565`
2. Connect your client to `localhost:25566`

The proxy also starts with a filter enabled to avoid logging more information than you need.<br>
To disable this filter run `filter disable`

## Commands
`stop`: Stops the proxy and compresses the log file<br>
`pause`: pauses the logging of all packets regardless of filter<br>
`resume`: resumes logging of packets after `pause` has been used<br>
`filter <allow|deny> <packet name>`: enables or disables logging the packet specified, packet name should be the packets name as given in the packet enums in `quartz_net`<br>
`filter <enable|disable>`: enables or disables the filter
`filter <reset>`: resets the filter, clearing all entries<br>
`filter <reset-as> <whitelist|blacklist>`: clears the filter and changes it to either act as a whitelist or blacklist<br>
`filter display`: logs the current filter<br>
`varint [bytes...]`: decodes the given bytes as a varint, bytes should be space seperated and in hex<br>
`string [bytes...]`: decodes the given bytes as a string, bytes should be space seperated<br>
`warnings <allow|suppress> <packet id>`: Enables/Disables supressing warnings related to the given packet id, packet ids should be in hex<br>
`warnings <on|off>`: enables or disables warnings related to parsing<br>
`mbdl <length>`: changes the maximum length allowed when displaying packet buffers in the console, `mbdl none` will disable the limit<br>
`metrics`: shows the approximate rate of packets in megabits per second