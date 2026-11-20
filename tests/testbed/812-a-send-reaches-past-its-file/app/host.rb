class Host
  include Mixin
  def plain = 3
end
Host.new.route(:x)
