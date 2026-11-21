class Lowered
  def shine
    1
  end

  def go(name)
    public_send(name)
  end
end

Lowered.new.go(:x)
