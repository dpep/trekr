class Spread
  def go = aa + bb + cc + rm
  def aa = 1
  def bb = 1
  def cc = 1
  def rm = 1
  private :aa,
    :bb
  alias_method :cc_new,
    :cc
  remove_method :rm
end
Spread.new.go
