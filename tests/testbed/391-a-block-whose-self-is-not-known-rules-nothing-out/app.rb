class Registry
  def self.register(&block); end
end

class Base
  def self.configure(&block); end
end

class Widget < Base
  configure do
    setup_widget
  end

  Registry.register do
    setup_widget
  end

  tap do
    setup_widget
  end

  def self.build(items)
    items.each { setup_widget }
    [1].map { setup_widget }
  end

  def setup_widget; end
end
