class Builder
  def verify_uri(state:); end
  def store(item); end
end

class Ledger
  def verify_uri(state:); end
  def store(item); end
end

class Mailer
  attr_reader :ledger
  attr_accessor :spare

  def initialize
    @ledger = Ledger.new
    @spare = Ledger.new
    @mixed = Ledger.new
  end

  def link(id)
    builder.verify_uri(state: id)
  end

  def fresh(id)
    plain.verify_uri(state: id)
  end

  def built(id)
    factory.verify_uri(state: id)
  end

  def keep(item)
    ledger.store(item)
  end

  def spared(item)
    spare.store(item)
  end

  def mixed_up(item)
    mixed.store(item)
  end

  def memo(item)
    @maker.store(item)
  end

  def boot(item)
    @app.store(item)
  end

  def reset
    @ledger = nil
    @mixed = Builder.new
  end

  private

  def builder
    @maker ||= Builder.new
  end

  def plain
    Builder.new
  end

  def factory
    Builder.new
  end

  def mixed
    @mixed
  end

  def app
    @app ||= Class.new(Ledger)
  end
end

class Express < Mailer
  def factory
    Ledger.new
  end
end
