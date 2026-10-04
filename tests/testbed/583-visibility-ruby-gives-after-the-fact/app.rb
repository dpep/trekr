class Widget
  def self.a1; end
  def self.a2; end
  def self.a3; end
  private_class_method [:a1, :a2]
  private_class_method(*%i[a3])

  def shown; end
  def hidden; end
  private :hidden

  attr_reader :mode

  private

  alias_method :mode?, :mode
  define_method(:built) {}
  def inner; end

  public

  alias inner2 inner

  class << self
    def b1; end
    private :b1
  end
end
