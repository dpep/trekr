class Unbracketed
  def a(k, v) = Lever.public_send k, v
  def b(k) = Crank.new.try k
  def c(k) = Pulley.public_send (k)
end
Unbracketed.new.a(1, 2)
Unbracketed.new.b(1)
Unbracketed.new.c(1)
