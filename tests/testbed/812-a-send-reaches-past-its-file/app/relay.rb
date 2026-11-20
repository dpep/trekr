class Relay
  def go = Valve.new.send(:public_send, :relayed)
  def again = Valve.new.send(:public_send, :relayed)
  def hide = Valve.new.send(:public_send, :hidden)
end
Relay.new.go
Relay.new.again
Relay.new.hide
