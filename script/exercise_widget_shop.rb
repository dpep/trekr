# Drive widget_shop's app code, so the gold set records real dispatches in it.
#
#     TREKR_EXERCISE=script/exercise_widget_shop.rb \
#       bin/rails runner script/trace_gold.rb
#
# One exerciser per app: the harness is general, the paths worth walking are
# not. This one touches every shape the corpus was written to carry — an ivar
# typed by a constructor argument, an AR finder, a delegate, a concern's
# class-level methods, and a plain PORO — and nothing else, so the gold set
# stays about app code rather than about Rails booting.
#
# Records are built here rather than assumed, and cleaned up after, so the run
# is repeatable against a database that starts empty or does not.

require "securerandom"

Rails.application.eager_load!

tag = SecureRandom.hex(4)
supplier = Supplier.create!(name: "domestic")
cheap = Widget.create!(name: "cheap-#{tag}", price_cents: 500, status: :active, supplier: supplier)
dear = Widget.create!(name: "dear-#{tag}", price_cents: 5_000, status: :retired, supplier: supplier)
order = Order.create!(widget: cheap, quantity: 60, reference: "R-#{tag}")

# PricingService: ivar receiver from a constructor param, delegate call, enum
# predicate, scope call, private self-calls, constant reference.
pricing = PricingService.new(cheap)
pricing.quote(60)          # bulk?, BULK_THRESHOLD, discount_rate, retired?
pricing.quote(1)           # the non-bulk return
pricing.region_surcharge   # supplier_region, the prefixed delegate
pricing.competitive?       # Widget.affordable.exists?
PricingService.new(dear).quote(60)

# OrderFulfillment: cross-class PORO calls, prefixed delegate, association
# chains, a memoized ivar, and a module-function stub.
fulfillment = OrderFulfillment.new(order)
fulfillment.process        # @pricing.quote, ledger.record, notify_supplier
fulfillment.summary        # @order.widget_name, @order.supplier.name

draft = Widget.create!(name: "draft-#{tag}", price_cents: 100, status: :draft, supplier: supplier)
OrderFulfillment.new(Order.create!(widget: draft, quantity: 1, reference: "D-#{tag}")).process

# Ledger and the stub directly, so their own bodies are traced too.
ledger = Ledger.new
ledger.record("X-#{tag}", 1)
ledger.entries
SupplierMailerStub.deliver("n", "r")

# Auditable: `included do` ran on save above; drive the class_methods block and
# the instance methods that go through `self.class`.
Widget.audited_count
Widget.audit_log
cheap.audited?
cheap.record_audit

# RestockJob: AR finders, an enum scope, block-param receivers, association
# create!.
job = RestockJob.new
job.perform(cheap.id)      # Widget.find, active?, Supplier.find_by
job.perform(dear.id)       # falls through to reorder → orders.create!
job.backlog                # Widget.retired.map { |w| w.orders.count }

Order.where(reference: ["R-#{tag}", "D-#{tag}"]).destroy_all
Order.where(widget_id: [cheap.id, dear.id, draft.id]).destroy_all
[cheap, dear, draft].each(&:destroy)
supplier.destroy
