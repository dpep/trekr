class Router
  def self.draw(&blk)
    new.instance_eval(&blk)
  end

  def get(path); path; end
end

class Printer
  def self.run
    io = Object.new
    def io.flush
      Printer.instance_exec(1) { |x| helper_three }
      Router.draw do
        get "/a"
      end
      [1].each { helper_four }
    end
    io.flush
  end

  def self.helper_three; 3; end
  def self.helper_four; 4; end
end
Printer.run
