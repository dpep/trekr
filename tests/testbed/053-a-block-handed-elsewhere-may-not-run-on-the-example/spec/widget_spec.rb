module Builder
  def self.build(&block)
    Class.new(&block)
  end
end

RSpec.describe Widget do
  def assemble(&block)
    instance_eval(&block)
  end

  def part
  end

  it "builds" do
    expect { part }
    [1].each { part }
    Dir.mktmpdir { part }
    Builder.build { part }
    assemble { part }
    Class.new { part }
  end
end
