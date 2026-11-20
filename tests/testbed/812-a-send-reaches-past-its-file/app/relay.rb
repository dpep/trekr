class Relay
  def go = Valve.new.send(:public_send, :relayed)
end
Relay.new.go
