class Poker
  def a = Grid.new.send(:%, 1)
  def b = Grid.new.public_send(:+, 1)
  def c = Grid.new.send("spelled")
end
Poker.new.a
Poker.new.b
Poker.new.c
