class Pourer
  def go = Spout.new.send("tip")
end
Pourer.new.go
