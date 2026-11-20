class Cite
  def go(k, a)
    m = "handle_#{k}"
    m = :other_z if a
    public_send(m)
  end

  def other_z = 1
end
Cite.new.go(1, true)

class Shifter
  def nilled(k, c)
    m = "default_#{k}"
    m = nil
    m = "handle_#{k}" if c
    VatA.new.public_send(m)
  end

  def reshaped(k, c)
    m = "handle_#{k}"
    m = "spare_#{k}" if c
    VatB.new.public_send(m)
  end

  def param(m, k, c)
    m = "handle_#{k}" if c
    VatC.new.public_send(m)
  end
end
s = Shifter.new
s.nilled(1, true)
s.reshaped(1, true)
s.param(:x, 1, true)
