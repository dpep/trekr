class Policy
  extend T::Sig

  sig { params(object: Widget, context: ::Lib::Context).returns(T::Boolean) }
  def self.authorized?(object, context)
    context.audit
    puts context.locale
    return true if context.admin?
    false
  end

  sig { returns(Lib::Context) }
  def self.current
  end

  def self.chained
    current.admin?
  end

  def self.found
    context = Lib::Context.find(1)
    context.admin?
  end

  def self.made
    Lib::Context.new.admin?
    context = Lib::Context.new
    context.admin?
    Account.new.admin?
    App::Context.new.admin?
  end
end
