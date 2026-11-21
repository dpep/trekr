class Loose
  validate :check

  def run
    Steps.new.go
  end

  def check
    Steps.new.verify
  end

  def relay(name)
    send(name)
  end
end
