class Inlined
  def named = 1
  def caller = named
  def build = send("named")
end
Inlined.new.caller
