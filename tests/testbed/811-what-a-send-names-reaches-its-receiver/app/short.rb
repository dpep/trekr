class Short
  def direct(k, v) = Hinge.public_send("#{k}=", v)
  def made(k) = Latch.new.public_send("#{k}?")
  def handle(n) = Bolt.method(n).call
  def public_handle(n) = Rivet.new.public_method(n).call
  def passed(n) = [1].map(&Screw.method(n))
end
Short.new.direct(1, 2)
Short.new.made(1)
Short.new.handle(1)
Short.new.public_handle(1)
Short.new.passed(1)
